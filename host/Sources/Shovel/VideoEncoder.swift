import AVFoundation
import CoreVideo
import Foundation
import ImageIO
import UniformTypeIdentifiers

enum VideoEncoder {
    static func run(_ args: [String]) -> Int32 {
        func value(_ flag: String) -> String? {
            guard let index = args.firstIndex(of: flag), index + 1 < args.count else { return nil }
            return args[index + 1]
        }
        guard let out = value("--encode"),
              let width = Int(value("--width") ?? ""),
              let height = Int(value("--height") ?? ""),
              let fps = Int32(value("--fps") ?? "10")
        else {
            FileHandle.standardError.write(Data("usage: Shovel --encode out.mp4 --width W --height H [--fps N] [--gif out.gif]\n".utf8))
            return 2
        }
        do {
            let url = URL(fileURLWithPath: out)
            let frames = try encode(to: url, width: width, height: height, fps: fps)
            var result: [String: Any] = ["mp4": url.path, "frames": frames, "seconds": Double(frames) / Double(fps)]
            if let gif = value("--gif") {
                let gifWidth = Int(value("--gif-width") ?? "") ?? 960
                let gifFps = Int(value("--gif-fps") ?? "") ?? 6
                try makeGIF(from: url, to: URL(fileURLWithPath: gif), width: gifWidth, fps: gifFps)
                result["gif"] = gif
            }
            let data = try JSONSerialization.data(withJSONObject: result)
            FileHandle.standardOutput.write(data)
            FileHandle.standardOutput.write(Data("\n".utf8))
            return 0
        } catch {
            FileHandle.standardError.write(Data("encode failed: \(describe(error))\n".utf8))
            return 1
        }
    }

    static func readFull(_ buffer: UnsafeMutableRawPointer, _ count: Int) -> Bool {
        var offset = 0
        while offset < count {
            let n = read(0, buffer + offset, count - offset)
            if n <= 0 { return false }
            offset += n
        }
        return true
    }

    static func encode(to url: URL, width: Int, height: Int, fps: Int32) throws -> Int64 {
        try? FileManager.default.removeItem(at: url)
        let writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: 4_000_000],
        ])
        input.expectsMediaDataInRealTime = false
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: width,
            kCVPixelBufferHeightKey as String: height,
        ])
        writer.add(input)
        guard writer.startWriting() else { throw writer.error ?? ShovelError("cannot start writer") }
        writer.startSession(atSourceTime: .zero)

        let rowBytes = width * 4
        let frame = UnsafeMutableRawPointer.allocate(byteCount: rowBytes * height, alignment: 16)
        defer { frame.deallocate() }
        var index: Int64 = 0
        while readFull(frame, rowBytes * height) {
            while !input.isReadyForMoreMediaData { usleep(2000) }
            guard let pool = adaptor.pixelBufferPool else { throw writer.error ?? ShovelError("no pixel buffer pool") }
            var pixelBuffer: CVPixelBuffer?
            CVPixelBufferPoolCreatePixelBuffer(nil, pool, &pixelBuffer)
            guard let buffer = pixelBuffer else { throw ShovelError("cannot allocate pixel buffer") }
            CVPixelBufferLockBaseAddress(buffer, [])
            let base = CVPixelBufferGetBaseAddress(buffer)!
            let stride = CVPixelBufferGetBytesPerRow(buffer)
            for row in 0..<height {
                (base + row * stride).copyMemory(from: frame + row * rowBytes, byteCount: rowBytes)
            }
            CVPixelBufferUnlockBaseAddress(buffer, [])
            adaptor.append(buffer, withPresentationTime: CMTime(value: index, timescale: fps))
            index += 1
        }
        input.markAsFinished()
        let done = DispatchSemaphore(value: 0)
        writer.finishWriting { done.signal() }
        done.wait()
        if writer.status != .completed {
            throw writer.error ?? ShovelError("video writer ended with status \(writer.status.rawValue)")
        }
        if index == 0 { throw ShovelError("no frames were recorded") }
        return index
    }

    static func makeGIF(from video: URL, to gif: URL, width: Int, fps: Int) throws {
        let asset = AVURLAsset(url: video)
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.requestedTimeToleranceBefore = .zero
        generator.requestedTimeToleranceAfter = .zero
        generator.maximumSize = CGSize(width: width, height: width)

        let durationSemaphore = DispatchSemaphore(value: 0)
        var seconds = 0.0
        Task {
            seconds = (try? await asset.load(.duration).seconds) ?? 0
            durationSemaphore.signal()
        }
        durationSemaphore.wait()

        let count = max(1, Int(seconds * Double(fps)))
        let times = (0..<count).map { NSValue(time: CMTime(value: CMTimeValue($0), timescale: CMTimeScale(fps))) }
        try? FileManager.default.removeItem(at: gif)
        guard let destination = CGImageDestinationCreateWithURL(gif as CFURL, UTType.gif.identifier as CFString, count, nil) else {
            throw ShovelError("cannot create \(gif.path)")
        }
        CGImageDestinationSetProperties(destination, [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFLoopCount: 0]] as CFDictionary)
        let frameProperties = [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFDelayTime: 1.0 / Double(fps)]] as CFDictionary

        let finished = DispatchSemaphore(value: 0)
        var remaining = count
        var lastImage: CGImage?
        let lock = NSLock()
        generator.generateCGImagesAsynchronously(forTimes: times) { _, image, _, _, _ in
            lock.lock()
            if let image = image ?? lastImage {
                CGImageDestinationAddImage(destination, image, frameProperties)
                lastImage = image
            }
            remaining -= 1
            let done = remaining == 0
            lock.unlock()
            if done { finished.signal() }
        }
        finished.wait()
        guard CGImageDestinationFinalize(destination) else { throw ShovelError("cannot finalize \(gif.path)") }
    }
}
