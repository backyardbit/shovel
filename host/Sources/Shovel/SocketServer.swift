import Foundation

final class ResponseBox {
    var value: [String: Any] = [:]
}

final class SocketServer {
    typealias Handler = ([String: Any], @escaping ([String: Any]) -> Void) -> Void

    let path: String
    let handler: Handler

    init(path: String, handler: @escaping Handler) {
        self.path = path
        self.handler = handler
    }

    func start() throws {
        if isAlive() {
            throw ShovelError("another Shovel host is already listening on \(path)")
        }
        unlink(path)
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw ShovelError("socket() failed: \(errno)") }
        var addr = Self.address(path)
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bound == 0 else { throw ShovelError("bind(\(path)) failed: \(errno)") }
        chmod(path, 0o600)
        guard listen(fd, 32) == 0 else { throw ShovelError("listen() failed: \(errno)") }
        let thread = Thread { [weak self] in
            while true {
                let client = accept(fd, nil, nil)
                if client < 0 { continue }
                Thread.detachNewThread { self?.serve(client) }
            }
        }
        thread.start()
    }

    static func address(_ path: String) -> sockaddr_un {
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        withUnsafeMutableBytes(of: &addr.sun_path) { buffer in
            for (index, byte) in bytes.prefix(buffer.count - 1).enumerated() {
                buffer[index] = byte
            }
        }
        return addr
    }

    func isAlive() -> Bool {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }
        var addr = Self.address(path)
        let result = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        return result == 0
    }

    private func serve(_ fd: Int32) {
        defer { close(fd) }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 65536)
        while !data.contains(10) {
            let count = read(fd, &buffer, buffer.count)
            if count <= 0 { break }
            data.append(buffer, count: count)
        }
        let line = data.split(separator: 10).first ?? Data()
        var response: [String: Any] = ["ok": false, "error": "malformed request"]
        if let request = (try? JSONSerialization.jsonObject(with: Data(line))) as? [String: Any] {
            let semaphore = DispatchSemaphore(value: 0)
            let box = ResponseBox()
            DispatchQueue.main.async {
                self.handler(request) { reply in
                    box.value = reply
                    semaphore.signal()
                }
            }
            semaphore.wait()
            response = box.value
        }
        var out = (try? JSONSerialization.data(withJSONObject: response)) ?? Data("{\"ok\":false}".utf8)
        out.append(10)
        out.withUnsafeBytes { pointer in
            var offset = 0
            while offset < pointer.count {
                let written = write(fd, pointer.baseAddress! + offset, pointer.count - offset)
                if written <= 0 { break }
                offset += written
            }
        }
    }
}
