import AppKit
import Virtualization
import VNCShim

final class RunningVM: NSObject, VZVirtualMachineDelegate {
    let id: String
    let dir: URL
    let machine: VZVirtualMachine
    let config: VZVirtualMachineConfiguration
    let vncPassword: String
    var vnc: SHVNCServer?
    var vncPort = 0
    var controller = "agent"
    var viewer: ViewerWindowController?
    var onStop: ((RunningVM) -> Void)?
    var ip: String?
    var lastActive = Date()
    var suspending = false
    var processID: pid_t?
    var cpuSample: (seconds: Double, at: Date)?

    init(id: String, dir: URL, machine: VZVirtualMachine, config: VZVirtualMachineConfiguration) {
        self.id = id
        self.dir = dir
        self.machine = machine
        self.config = config
        let alphabet = Array("abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789")
        vncPassword = String((0..<8).map { _ in alphabet.randomElement()! })
        super.init()
    }

    var state: String {
        switch machine.state {
        case .stopped: return "stopped"
        case .running: return "running"
        case .paused: return "paused"
        case .error: return "error"
        case .starting: return "starting"
        case .pausing: return "pausing"
        case .resuming: return "resuming"
        case .stopping: return "stopping"
        case .saving: return "saving"
        case .restoring: return "restoring"
        @unknown default: return "unknown"
        }
    }

    var info: [String: Any] {
        [
            "id": id,
            "dir": dir.path,
            "state": state,
            "controller": controller,
            "vnc_port": vncPort,
            "vnc_password": vncPassword,
            "viewer_open": viewer?.window?.isVisible ?? false,
            "idle_seconds": Int(Date().timeIntervalSince(lastActive)),
        ]
    }

    func cpuLoad() -> Double {
        guard let pid = processID ?? findProcess() else { return .infinity }
        var usage = rusage_info_v4()
        let result = withUnsafeMutablePointer(to: &usage) {
            $0.withMemoryRebound(to: rusage_info_t?.self, capacity: 1) { proc_pid_rusage(pid, RUSAGE_INFO_V4, $0) }
        }
        guard result == 0 else {
            processID = nil
            return .infinity
        }
        var timebase = mach_timebase_info()
        mach_timebase_info(&timebase)
        let seconds = Double(usage.ri_user_time + usage.ri_system_time) * Double(timebase.numer) / Double(timebase.denom) / 1e9
        let now = Date()
        defer { cpuSample = (seconds, now) }
        guard let last = cpuSample, now.timeIntervalSince(last.at) > 1 else { return .infinity }
        return (seconds - last.seconds) / now.timeIntervalSince(last.at)
    }

    func findProcess() -> pid_t? {
        let disk = dir.appendingPathComponent("disk.img").path
        var pids = [pid_t](repeating: 0, count: 8192)
        let count = Int(proc_listallpids(&pids, Int32(pids.count * MemoryLayout<pid_t>.size)))
        for pid in pids.prefix(max(count, 0)) {
            var path = [CChar](repeating: 0, count: Int(MAXPATHLEN) * 4)
            guard proc_pidpath(pid, &path, UInt32(path.count)) > 0,
                  String(cString: path).hasSuffix("/com.apple.Virtualization.VirtualMachine") else { continue }
            let size = proc_pidinfo(pid, PROC_PIDLISTFDS, 0, nil, 0)
            guard size > 0 else { continue }
            var fds = [proc_fdinfo](repeating: proc_fdinfo(), count: Int(size) / MemoryLayout<proc_fdinfo>.size)
            let filled = Int(proc_pidinfo(pid, PROC_PIDLISTFDS, 0, &fds, size)) / MemoryLayout<proc_fdinfo>.size
            for fd in fds.prefix(filled) where fd.proc_fdtype == PROX_FDTYPE_VNODE {
                var info = vnode_fdinfowithpath()
                guard proc_pidfdinfo(pid, fd.proc_fd, PROC_PIDFDVNODEPATHINFO, &info, Int32(MemoryLayout<vnode_fdinfowithpath>.size)) > 0 else { continue }
                let open = withUnsafeBytes(of: info.pvip.vip_path) { String(cString: $0.bindMemory(to: CChar.self).baseAddress!) }
                if open == disk {
                    processID = pid
                    return pid
                }
            }
        }
        return nil
    }

    func isConnected(_ connections: [[String]]) -> Bool {
        connections.contains { pair in
            pair.contains { address in
                if let ip, address == "\(ip).22" { return true }
                return vncPort != 0 && (address == "127.0.0.1.\(vncPort)" || address == "::1.\(vncPort)")
            }
        }
    }

    func guestDidStop(_ virtualMachine: VZVirtualMachine) {
        onStop?(self)
    }

    func virtualMachine(_ virtualMachine: VZVirtualMachine, didStopWithError error: Error) {
        onStop?(self)
    }
}

final class Manager {
    static let idleCPU = 0.25
    var running: [String: RunningVM] = [:]
    var jobs: [String: InstallJob] = [:]
    var onChange: (() -> Void)?
    var idleTimer: Timer?

    init() {
        idleTimer = Timer.scheduledTimer(withTimeInterval: 20, repeats: true) { [weak self] _ in self?.checkIdle() }
    }

    func handle(_ request: [String: Any], reply: @escaping ([String: Any]) -> Void) {
        let command = request["cmd"] as? String ?? ""
        if ["start", "wake", "stop", "suspend"].contains(command), let id = request["id"] as? String, running[id]?.suspending == true {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { self.handle(request, reply: reply) }
            return
        }
        do {
            switch command {
            case "ping":
                reply(["ok": true, "pid": Int(getpid())])
            case "list":
                reply(["ok": true, "vms": running.values.map(\.info), "jobs": jobs.values.map(\.info)])
            case "status":
                let id = try string(request, "id")
                reply(["ok": true, "vm": running[id]?.info ?? ["id": id, "state": "stopped"]])
            case "start":
                try start(request, reply: reply)
            case "wake":
                try wake(request, reply: reply)
            case "suspend":
                let id = try string(request, "id")
                guard let vm = running[id] else {
                    reply(["ok": true, "vm": ["id": id, "state": "stopped"]])
                    return
                }
                try suspend(vm, reply: reply)
            case "stop":
                try stop(request, reply: reply)
            case "view":
                let vm = try runningVM(request)
                showViewer(vm)
                reply(["ok": true, "vm": vm.info])
            case "control":
                let vm = try runningVM(request)
                let controller = try string(request, "controller")
                guard controller == "agent" || controller == "human" else {
                    throw ShovelError("controller must be 'agent' or 'human'")
                }
                setController(vm, controller)
                reply(["ok": true, "vm": vm.info])
            case "scroll":
                let vm = try runningVM(request)
                let dx = request["dx"] as? Int ?? 0
                let dy = request["dy"] as? Int ?? 0
                guard abs(dx) <= 1000, abs(dy) <= 1000 else { throw ShovelError("scroll amount must be at most 1000 ticks") }
                guard SHSendScrollWheel(vm.machine, dx, dy) else {
                    throw ShovelError("this macOS version does not support scroll input to VMs")
                }
                reply(["ok": true])
            case "install":
                try install(request, reply: reply)
            case "job":
                let id = try string(request, "id")
                guard let job = jobs[id] else { throw ShovelError("no install job for \(id)") }
                reply(["ok": true, "job": job.info])
            case "quit":
                reply(["ok": true])
                suspendAll { NSApp.terminate(nil) }
            default:
                throw ShovelError("unknown command '\(command)'")
            }
        } catch {
            reply(["ok": false, "error": describe(error)])
        }
    }

    func string(_ request: [String: Any], _ key: String) throws -> String {
        guard let value = request[key] as? String, !value.isEmpty else { throw ShovelError("missing '\(key)'") }
        return value
    }

    func runningVM(_ request: [String: Any]) throws -> RunningVM {
        let id = try string(request, "id")
        guard let vm = running[id] else { throw ShovelError("\(id) is not running") }
        vm.lastActive = Date()
        return vm
    }

    func start(_ request: [String: Any], reply: @escaping ([String: Any]) -> Void) throws {
        let id = try string(request, "id")
        let dir = URL(fileURLWithPath: try string(request, "dir"))
        let view = request["view"] as? Bool ?? false
        let takeover = request["takeover"] as? Bool ?? false
        if let existing = running[id] {
            existing.lastActive = Date()
            if view { showViewer(existing) }
            reply(["ok": true, "vm": existing.info, "resumed": false])
            return
        }
        if request["fresh"] as? Bool ?? false {
            try? FileManager.default.removeItem(at: VMFactory.statePath(dir))
        }
        try launch(id: id, dir: dir, view: view, takeover: takeover, reply: reply)
    }

    func wake(_ request: [String: Any], reply: @escaping ([String: Any]) -> Void) throws {
        let id = try string(request, "id")
        if let vm = running[id] {
            vm.lastActive = Date()
            if let ip = request["ip"] as? String { vm.ip = ip }
            reply(["ok": true, "vm": vm.info, "resumed": false])
            return
        }
        let dir = URL(fileURLWithPath: try string(request, "dir"))
        guard FileManager.default.fileExists(atPath: VMFactory.statePath(dir).path) else {
            reply(["ok": true, "vm": ["id": id, "state": "stopped"], "resumed": false])
            return
        }
        try launch(id: id, dir: dir, view: false, takeover: false) { [weak self] result in
            if let ip = request["ip"] as? String { self?.running[id]?.ip = ip }
            reply(result)
        }
    }

    func launch(id: String, dir: URL, view: Bool, takeover: Bool, reply: @escaping ([String: Any]) -> Void) throws {
        let state = VMFactory.statePath(dir)
        let restoring = FileManager.default.fileExists(atPath: state.path)
        let kept = "The saved state \(state.path) was kept; run `shovel up \(id) --fresh` to discard it and cold boot."
        let spec = try VMFactory.loadSpec(dir)
        let config = try VMFactory.configuration(dir: dir, spec: spec, includeShares: true)
        if restoring {
            do {
                try config.validateSaveRestoreSupport()
            } catch {
                throw ShovelError("cannot resume \(id): \(sentence(error)) \(kept)")
            }
        }
        let machine = VZVirtualMachine(configuration: config)
        let vm = RunningVM(id: id, dir: dir, machine: machine, config: config)
        machine.delegate = vm
        vm.onStop = { [weak self] stopped in self?.remove(stopped) }
        running[id] = vm
        onChange?()

        let started = Date()
        let finish: (Error?) -> Void = { [weak self] error in
            guard let self else { return }
            if let error {
                if machine.canStop { machine.stop { _ in } }
                self.running[id] = nil
                self.onChange?()
                let message = restoring ? "could not resume \(id) from its saved state: \(sentence(error)) \(kept)" : "failed to start \(id): \(describe(error))"
                reply(["ok": false, "error": message])
                return
            }
            if restoring { try? FileManager.default.removeItem(at: state) }
            if let server = SHVNCServer(virtualMachine: machine, password: vm.vncPassword, queue: DispatchQueue.global()) {
                vm.vnc = server
                server.start()
            }
            if takeover { vm.controller = "human" }
            if view || takeover { self.showViewer(vm) }
            self.waitForVNC(vm, attempts: 50) {
                self.onChange?()
                reply(["ok": true, "vm": vm.info, "resumed": restoring, "seconds": Date().timeIntervalSince(started)])
            }
        }
        if restoring {
            machine.restoreMachineStateFrom(url: state) { error in
                if let error { return finish(error) }
                machine.resume { result in
                    if case .failure(let error) = result { finish(error) } else { finish(nil) }
                }
            }
        } else {
            machine.start { result in
                if case .failure(let error) = result { finish(error) } else { finish(nil) }
            }
        }
    }

    func suspend(_ vm: RunningVM, reply: @escaping ([String: Any]) -> Void) throws {
        guard !vm.id.hasPrefix("image:") else { throw ShovelError("images cannot be suspended; use `shovel image stop`") }
        guard vm.machine.state == .running else { throw ShovelError("\(vm.id) is \(vm.state) and cannot be suspended now") }
        do {
            try vm.config.validateSaveRestoreSupport()
        } catch {
            throw ShovelError("\(vm.id) cannot be suspended: \(describe(error))")
        }
        let state = VMFactory.statePath(vm.dir)
        let partial = vm.dir.appendingPathComponent("state.partial.vzvmsave")
        try? FileManager.default.removeItem(at: partial)
        vm.suspending = true
        onChange?()
        let started = Date()
        let fail: (Error) -> Void = { [weak self] error in
            try? FileManager.default.removeItem(at: partial)
            vm.suspending = false
            if vm.machine.state == .paused { vm.machine.resume { _ in } }
            self?.onChange?()
            let hint = Manager.screenLocked ? " The Mac's screen is locked; macOS only creates the key that encrypts saved VM state while it is unlocked." : ""
            reply(["ok": false, "error": "could not suspend \(vm.id): \(describe(error))\(hint)"])
        }
        vm.machine.pause { result in
            if case .failure(let error) = result { return fail(error) }
            vm.machine.saveMachineStateTo(url: partial) { error in
                if let error { return fail(error) }
                vm.machine.stop { [weak self] error in
                    if let error { return fail(error) }
                    do {
                        try? FileManager.default.removeItem(at: state)
                        try FileManager.default.moveItem(at: partial, to: state)
                    } catch {
                        try? FileManager.default.removeItem(at: partial)
                        self?.remove(vm)
                        reply(["ok": false, "error": "\(vm.id) was stopped but its saved state could not be kept: \(describe(error))"])
                        return
                    }
                    self?.remove(vm)
                    let bytes = (try? FileManager.default.attributesOfItem(atPath: state.path)[.size] as? Int) ?? 0
                    reply(["ok": true, "vm": ["id": vm.id, "state": "suspended"], "seconds": Date().timeIntervalSince(started), "state_bytes": bytes])
                }
            }
        }
    }

    func suspendAll(then done: @escaping () -> Void) {
        let vms = running.values.filter { !$0.id.hasPrefix("image:") && $0.machine.state == .running && !$0.suspending }
        var finished = false
        let finish = {
            if finished { return }
            finished = true
            done()
        }
        let group = DispatchGroup()
        for vm in vms {
            group.enter()
            do {
                try suspend(vm) { _ in group.leave() }
            } catch {
                group.leave()
            }
        }
        group.notify(queue: .main, execute: finish)
        DispatchQueue.main.asyncAfter(deadline: .now() + 120, execute: finish)
    }

    static var screenLocked: Bool {
        let session = CGSessionCopyCurrentDictionary() as? [String: Any]
        return session?["CGSSessionScreenIsLocked"] as? Bool ?? false
    }

    func checkIdle() {
        let candidates = running.values.filter { !$0.suspending && $0.machine.state == .running && !$0.id.hasPrefix("image:") }
        guard !candidates.isEmpty else { return }
        let connections = establishedConnections()
        for vm in candidates {
            let load = vm.cpuLoad()
            if load > Manager.idleCPU || vm.viewer?.window?.isVisible == true || vm.controller == "human" || vm.isConnected(connections) {
                vm.lastActive = Date()
            }
            guard let minutes = (try? VMFactory.loadSpec(vm.dir))?.idle_suspend_minutes, minutes > 0,
                  Date().timeIntervalSince(vm.lastActive) >= Double(minutes * 60) else { continue }
            do {
                try suspend(vm) { reply in
                    if reply["ok"] as? Bool != true { vm.lastActive = Date() }
                }
            } catch {
                vm.lastActive = Date()
            }
        }
    }

    func establishedConnections() -> [[String]] {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/netstat")
        process.arguments = ["-an"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        guard (try? process.run()) != nil else { return [] }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return String(decoding: data, as: UTF8.self).split(separator: "\n").compactMap { line in
            let fields = line.split(separator: " ").map(String.init)
            return fields.count >= 6 && fields[0].hasPrefix("tcp") && fields[5] == "ESTABLISHED" ? [fields[3], fields[4]] : nil
        }
    }

    func waitForVNC(_ vm: RunningVM, attempts: Int, done: @escaping () -> Void) {
        let port = vm.vnc?.port() ?? 0
        if port != 0 || attempts <= 0 || vm.vnc == nil {
            vm.vncPort = port
            done()
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
            self.waitForVNC(vm, attempts: attempts - 1, done: done)
        }
    }

    func stop(_ request: [String: Any], reply: @escaping ([String: Any]) -> Void) throws {
        let id = try string(request, "id")
        guard let vm = running[id] else {
            reply(["ok": true, "vm": ["id": id, "state": "stopped"]])
            return
        }
        let wait = request["wait"] as? Double ?? 0
        let deadline = Date().addingTimeInterval(wait)
        waitForStop(vm, deadline: deadline) {
            reply(["ok": true, "vm": ["id": id, "state": "stopped"]])
        }
    }

    func waitForStop(_ vm: RunningVM, deadline: Date, done: @escaping () -> Void) {
        if vm.machine.state == .stopped || vm.machine.state == .error {
            remove(vm)
            done()
            return
        }
        if Date() >= deadline {
            if vm.machine.canStop {
                vm.machine.stop { [weak self] _ in
                    self?.remove(vm)
                    done()
                }
            } else {
                remove(vm)
                done()
            }
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            self.waitForStop(vm, deadline: deadline, done: done)
        }
    }

    func remove(_ vm: RunningVM) {
        guard running[vm.id] === vm else { return }
        vm.vnc?.stop()
        vm.vnc = nil
        vm.viewer?.close()
        vm.viewer = nil
        running[vm.id] = nil
        updateActivationPolicy()
        onChange?()
    }

    func setController(_ vm: RunningVM, _ controller: String) {
        vm.controller = controller
        vm.viewer?.refresh()
        if controller == "human" { showViewer(vm) }
        onChange?()
    }

    func showViewer(_ vm: RunningVM) {
        if vm.viewer == nil {
            let viewer = ViewerWindowController(vm: vm)
            viewer.onToggle = { [weak self, weak vm] in
                guard let self, let vm else { return }
                self.setController(vm, vm.controller == "human" ? "agent" : "human")
            }
            viewer.onClose = { [weak self, weak vm] in
                guard let self, let vm else { return }
                vm.viewer = nil
                if vm.controller == "human" { self.setController(vm, "agent") }
                self.updateActivationPolicy()
            }
            vm.viewer = viewer
        }
        NSApp.setActivationPolicy(.regular)
        DispatchQueue.main.async {
            NSApp.activate()
            vm.viewer?.showWindow(nil)
            vm.viewer?.window?.makeKeyAndOrderFront(nil)
            self.onChange?()
        }
    }

    func updateActivationPolicy() {
        let anyOpen = running.values.contains { $0.viewer != nil }
        NSApp.setActivationPolicy(anyOpen ? .regular : .accessory)
    }
}
