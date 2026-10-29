import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    let manager = Manager()
    var server: SocketServer?
    var statusItem: NSStatusItem?

    func applicationDidFinishLaunching(_ notification: Notification) {
        do {
            try FileManager.default.createDirectory(at: Paths.root, withIntermediateDirectories: true)
            let server = SocketServer(path: Paths.socket.path) { [manager] request, reply in
                manager.handle(request, reply: reply)
            }
            try server.start()
            self.server = server
        } catch {
            FileHandle.standardError.write(Data("shovel host: \(error)\n".utf8))
            exit(1)
        }

        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.image = NSImage(systemSymbolName: "macwindow.on.rectangle", accessibilityDescription: "Shovel")
        statusItem = item
        manager.onChange = { [weak self] in self?.rebuildMenu() }
        rebuildMenu()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    func applicationWillTerminate(_ notification: Notification) {
        unlink(Paths.socket.path)
    }

    func rebuildMenu() {
        let menu = NSMenu()
        let running = manager.running.values.sorted { $0.id < $1.id }
        if running.isEmpty {
            menu.addItem(withTitle: "No VMs running", action: nil, keyEquivalent: "")
        }
        for vm in running {
            let item = NSMenuItem(title: "\(vm.id) — \(vm.controller)", action: #selector(openViewer(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = vm.id
            menu.addItem(item)
        }
        for job in manager.jobs.values where !job.done {
            menu.addItem(withTitle: "Installing \(job.id): \(job.phase) \(Int(job.progress * 100))%", action: nil, keyEquivalent: "")
        }
        menu.addItem(.separator())
        let quit = NSMenuItem(title: "Quit Shovel (stops all VMs)", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        menu.addItem(quit)
        statusItem?.menu = menu
    }

    @objc func openViewer(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String, let vm = manager.running[id] else { return }
        manager.showViewer(vm)
    }
}
