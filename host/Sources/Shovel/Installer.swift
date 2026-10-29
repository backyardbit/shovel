import Foundation
import Virtualization

final class InstallJob {
    let id: String
    var phase = "starting"
    var progress = 0.0
    var error: String?
    var done = false
    var observers: [NSKeyValueObservation] = []
    var machine: VZVirtualMachine?
    var installer: VZMacOSInstaller?

    init(id: String) { self.id = id }

    var info: [String: Any] {
        var info: [String: Any] = ["id": id, "phase": phase, "progress": progress, "done": done]
        if let error { info["error"] = error }
        return info
    }
}

extension Manager {
    func install(_ request: [String: Any], reply: @escaping ([String: Any]) -> Void) throws {
        let id = try string(request, "id")
        let dir = URL(fileURLWithPath: try string(request, "dir"))
        let ipsw = request["ipsw"] as? String ?? "latest"
        let diskGB = request["disk_gb"] as? Int ?? 100
        if let existing = jobs[id], !existing.done {
            reply(["ok": true, "job": existing.info])
            return
        }
        let job = InstallJob(id: id)
        jobs[id] = job
        reply(["ok": true, "job": job.info])
        onChange?()

        if ipsw == "latest" {
            job.phase = "fetching latest restore image info"
            VZMacOSRestoreImage.fetchLatestSupported { result in
                DispatchQueue.main.async {
                    switch result {
                    case .success(let image): self.download(job, remote: image.url, dir: dir, diskGB: diskGB)
                    case .failure(let error): self.fail(job, error)
                    }
                }
            }
        } else {
            load(job, ipsw: URL(fileURLWithPath: ipsw), dir: dir, diskGB: diskGB)
        }
    }

    func fail(_ job: InstallJob, _ error: Error) {
        job.error = describe(error)
        job.phase = "failed"
        job.done = true
        job.machine = nil
        job.installer = nil
        job.observers = []
        onChange?()
    }

    func download(_ job: InstallJob, remote: URL, dir: URL, diskGB: Int) {
        let cached = Paths.ipswCache.appendingPathComponent(remote.lastPathComponent)
        if FileManager.default.fileExists(atPath: cached.path) {
            load(job, ipsw: cached, dir: dir, diskGB: diskGB)
            return
        }
        job.phase = "downloading \(remote.lastPathComponent)"
        let task = URLSession.shared.downloadTask(with: remote) { temporary, _, error in
            guard let temporary else {
                DispatchQueue.main.async { self.fail(job, error ?? ShovelError("download failed")) }
                return
            }
            do {
                try FileManager.default.createDirectory(at: Paths.ipswCache, withIntermediateDirectories: true)
                try FileManager.default.moveItem(at: temporary, to: cached)
                DispatchQueue.main.async { self.load(job, ipsw: cached, dir: dir, diskGB: diskGB) }
            } catch {
                DispatchQueue.main.async { self.fail(job, error) }
            }
        }
        job.observers.append(task.progress.observe(\.fractionCompleted) { progress, _ in
            DispatchQueue.main.async { job.progress = progress.fractionCompleted }
        })
        task.resume()
    }

    func load(_ job: InstallJob, ipsw: URL, dir: URL, diskGB: Int) {
        job.phase = "loading restore image"
        job.progress = 0
        VZMacOSRestoreImage.load(from: ipsw) { result in
            DispatchQueue.main.async {
                do {
                    let image = try result.get()
                    guard let requirements = image.mostFeaturefulSupportedConfiguration else {
                        throw ShovelError("\(ipsw.lastPathComponent) cannot be installed on this host")
                    }
                    try self.prepareBundle(dir: dir, model: requirements.hardwareModel, diskGB: diskGB)
                    var spec = try VMFactory.loadSpec(dir)
                    spec.cpus = max(spec.cpus, requirements.minimumSupportedCPUCount)
                    spec.memory_mb = max(spec.memory_mb, Int(requirements.minimumSupportedMemorySize >> 20))
                    let config = try VMFactory.configuration(dir: dir, spec: spec, includeShares: false)
                    let machine = VZVirtualMachine(configuration: config)
                    let installer = VZMacOSInstaller(virtualMachine: machine, restoringFromImageAt: ipsw)
                    job.machine = machine
                    job.installer = installer
                    job.phase = "installing macOS"
                    job.observers.append(installer.progress.observe(\.fractionCompleted) { progress, _ in
                        DispatchQueue.main.async { job.progress = progress.fractionCompleted }
                    })
                    installer.install { result in
                        DispatchQueue.main.async {
                            switch result {
                            case .success:
                                job.phase = "done"
                                job.progress = 1
                                job.done = true
                                job.machine = nil
                                job.installer = nil
                                job.observers = []
                                self.onChange?()
                            case .failure(let error):
                                self.fail(job, error)
                            }
                        }
                    }
                } catch {
                    self.fail(job, error)
                }
            }
        }
    }

    func prepareBundle(dir: URL, model: VZMacHardwareModel, diskGB: Int) throws {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try model.dataRepresentation.write(to: dir.appendingPathComponent("hardware.bin"))
        _ = try VZMacAuxiliaryStorage(creatingStorageAt: dir.appendingPathComponent("aux.img"), hardwareModel: model, options: [.allowOverwrite])
        let disk = dir.appendingPathComponent("disk.img")
        FileManager.default.createFile(atPath: disk.path, contents: nil)
        let handle = try FileHandle(forWritingTo: disk)
        try handle.truncate(atOffset: UInt64(diskGB) << 30)
        try handle.close()
        try? FileManager.default.removeItem(at: dir.appendingPathComponent("machine.bin"))
        _ = try VMFactory.machineIdentifier(dir)
    }
}
