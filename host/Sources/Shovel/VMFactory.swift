import Foundation
import Virtualization

struct DisplaySpec: Codable {
    var width: Int
    var height: Int
    var ppi: Int
}

struct ShareSpec: Codable {
    var name: String
    var path: String
    var read_only: Bool
}

struct VMSpec: Codable {
    var cpus: Int
    var memory_mb: Int
    var display: DisplaySpec
    var mac: String
    var shares: [ShareSpec]
}

enum VMFactory {
    static func loadSpec(_ dir: URL) throws -> VMSpec {
        let url = dir.appendingPathComponent("config.json")
        guard let data = try? Data(contentsOf: url) else {
            throw ShovelError("missing \(url.path)")
        }
        return try JSONDecoder().decode(VMSpec.self, from: data)
    }

    static func hardwareModel(_ dir: URL) throws -> VZMacHardwareModel {
        let url = dir.appendingPathComponent("hardware.bin")
        guard let data = try? Data(contentsOf: url), let model = VZMacHardwareModel(dataRepresentation: data) else {
            throw ShovelError("missing or invalid \(url.path)")
        }
        guard model.isSupported else { throw ShovelError("hardware model in \(url.path) is not supported on this host") }
        return model
    }

    static func machineIdentifier(_ dir: URL) throws -> VZMacMachineIdentifier {
        let url = dir.appendingPathComponent("machine.bin")
        if let data = try? Data(contentsOf: url), let identifier = VZMacMachineIdentifier(dataRepresentation: data) {
            return identifier
        }
        let identifier = VZMacMachineIdentifier()
        try identifier.dataRepresentation.write(to: url)
        return identifier
    }

    static func configuration(dir: URL, spec: VMSpec, includeShares: Bool) throws -> VZVirtualMachineConfiguration {
        let platform = VZMacPlatformConfiguration()
        platform.hardwareModel = try hardwareModel(dir)
        platform.machineIdentifier = try machineIdentifier(dir)
        platform.auxiliaryStorage = VZMacAuxiliaryStorage(contentsOf: dir.appendingPathComponent("aux.img"))

        let config = VZVirtualMachineConfiguration()
        config.platform = platform
        config.bootLoader = VZMacOSBootLoader()
        config.cpuCount = min(max(spec.cpus, VZVirtualMachineConfiguration.minimumAllowedCPUCount), VZVirtualMachineConfiguration.maximumAllowedCPUCount)
        config.memorySize = min(max(UInt64(spec.memory_mb) << 20, VZVirtualMachineConfiguration.minimumAllowedMemorySize), VZVirtualMachineConfiguration.maximumAllowedMemorySize)

        let graphics = VZMacGraphicsDeviceConfiguration()
        graphics.displays = [
            VZMacGraphicsDisplayConfiguration(widthInPixels: spec.display.width, heightInPixels: spec.display.height, pixelsPerInch: spec.display.ppi),
        ]
        config.graphicsDevices = [graphics]

        let disk = try VZDiskImageStorageDeviceAttachment(
            url: dir.appendingPathComponent("disk.img"),
            readOnly: false,
            cachingMode: .automatic,
            synchronizationMode: .full
        )
        config.storageDevices = [VZVirtioBlockDeviceConfiguration(attachment: disk)]

        let network = VZVirtioNetworkDeviceConfiguration()
        network.attachment = VZNATNetworkDeviceAttachment()
        guard let mac = VZMACAddress(string: spec.mac) else { throw ShovelError("invalid MAC address \(spec.mac)") }
        network.macAddress = mac
        config.networkDevices = [network]

        config.pointingDevices = [VZUSBScreenCoordinatePointingDeviceConfiguration(), VZMacTrackpadConfiguration()]
        config.keyboards = [VZUSBKeyboardConfiguration()]
        config.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]
        config.memoryBalloonDevices = [VZVirtioTraditionalMemoryBalloonDeviceConfiguration()]

        if includeShares && !spec.shares.isEmpty {
            var directories: [String: VZSharedDirectory] = [:]
            for share in spec.shares {
                directories[share.name] = VZSharedDirectory(url: URL(fileURLWithPath: share.path), readOnly: share.read_only)
            }
            let fs = VZVirtioFileSystemDeviceConfiguration(tag: VZVirtioFileSystemDeviceConfiguration.macOSGuestAutomountTag)
            fs.share = VZMultipleDirectoryShare(directories: directories)
            config.directorySharingDevices = [fs]
        }

        try config.validate()
        return config
    }
}
