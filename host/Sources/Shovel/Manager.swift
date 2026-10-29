import AppKit
import Virtualization
import VNCShim

final class RunningVM: NSObject, VZVirtualMachineDelegate {
    let id: String
    let dir: URL
    let machine: VZVirtualMachine
    let vncPassword: String
    var vnc: SHVNCServer?
    var vncPort = 0
    var controller = "agent"
    var viewer: ViewerWindowController?
    var onStop: ((RunningVM) -> Void)?

    init(id: String, dir: URL, machine: VZVirtualMachine) {
        self.id = id
        self.dir = dir
        self.machine = machine
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
        ]
    }

    func guestDidStop(_ virtualMachine: VZVirtualMachine) {
        onStop?(self)
    }

    func virtualMachine(_ virtualMachine: VZVirtualMachine, didStopWithError error: Error) {
        onStop?(self)
    }
}

final class Manager {
    var running: [String: RunningVM] = [:]
    var jobs: [String: InstallJob] = [:]
    var onChange: (() -> Void)?

    func handle(_ request: [String: Any], reply: @escaping ([String: Any]) -> Void) {
        let command = request["cmd"] as? String ?? ""
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
            case "install":
                try install(request, reply: reply)
            case "job":
                let id = try string(request, "id")
                guard let job = jobs[id] else { throw ShovelError("no install job for \(id)") }
                reply(["ok": true, "job": job.info])
            case "quit":
                reply(["ok": true])
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { NSApp.terminate(nil) }
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
        return vm
    }

    func start(_ request: [String: Any], reply: @escaping ([String: Any]) -> Void) throws {
        let id = try string(request, "id")
        let dir = URL(fileURLWithPath: try string(request, "dir"))
        let view = request["view"] as? Bool ?? false
        let takeover = request["takeover"] as? Bool ?? false
        if let existing = running[id] {
            if view { showViewer(existing) }
            reply(["ok": true, "vm": existing.info])
            return
        }
        let spec = try VMFactory.loadSpec(dir)
        let config = try VMFactory.configuration(dir: dir, spec: spec, includeShares: true)
        let machine = VZVirtualMachine(configuration: config)
        let vm = RunningVM(id: id, dir: dir, machine: machine)
        machine.delegate = vm
        vm.onStop = { [weak self] stopped in self?.remove(stopped) }
        running[id] = vm
        onChange?()

        machine.start { [weak self] result in
            guard let self else { return }
            switch result {
            case .failure(let error):
                self.running[id] = nil
                self.onChange?()
                reply(["ok": false, "error": "failed to start \(id): \(describe(error))"])
            case .success:
                if let server = SHVNCServer(virtualMachine: machine, password: vm.vncPassword, queue: DispatchQueue.global()) {
                    vm.vnc = server
                    server.start()
                }
                if takeover { vm.controller = "human" }
                if view || takeover { self.showViewer(vm) }
                self.waitForVNC(vm, attempts: 50) {
                    self.onChange?()
                    reply(["ok": true, "vm": vm.info])
                }
            }
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
        vm.viewer?.showWindow(nil)
        vm.viewer?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        onChange?()
    }

    func updateActivationPolicy() {
        let anyOpen = running.values.contains { $0.viewer != nil }
        NSApp.setActivationPolicy(anyOpen ? .regular : .accessory)
    }
}
