import AppKit
import Virtualization

final class ShieldView: NSView {
    override var acceptsFirstResponder: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { isHidden ? nil : super.hitTest(point) }
    override func mouseDown(with event: NSEvent) {}
    override func rightMouseDown(with event: NSEvent) {}
    override func otherMouseDown(with event: NSEvent) {}
    override func scrollWheel(with event: NSEvent) {}
    override func keyDown(with event: NSEvent) {}
    override func keyUp(with event: NSEvent) {}
    override func flagsChanged(with event: NSEvent) {}
}

final class ViewerWindowController: NSWindowController, NSWindowDelegate {
    let vm: RunningVM
    let machineView = VZVirtualMachineView()
    let shield = ShieldView()
    let toggleButton = NSButton(title: "", target: nil, action: nil)
    let statusLabel = NSTextField(labelWithString: "")
    var onToggle: (() -> Void)?
    var onClose: (() -> Void)?

    init(vm: RunningVM) {
        self.vm = vm
        let spec = try? VMFactory.loadSpec(vm.dir)
        let width = CGFloat(spec?.display.width ?? 1440)
        let height = CGFloat(spec?.display.height ?? 900)
        let scale = min(1, 1280 / width)
        let barHeight: CGFloat = 36
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: width * scale, height: height * scale + barHeight),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Shovel — \(vm.id)"
        window.contentAspectRatio = NSSize(width: width, height: height + barHeight / scale)
        super.init(window: window)
        window.delegate = self

        machineView.virtualMachine = vm.machine
        machineView.automaticallyReconfiguresDisplay = false
        machineView.translatesAutoresizingMaskIntoConstraints = false
        shield.translatesAutoresizingMaskIntoConstraints = false

        toggleButton.target = self
        toggleButton.action = #selector(toggle)
        toggleButton.bezelStyle = .rounded
        statusLabel.font = .systemFont(ofSize: 12, weight: .medium)

        let bar = NSStackView(views: [statusLabel, NSView(), toggleButton])
        bar.orientation = .horizontal
        bar.edgeInsets = NSEdgeInsets(top: 4, left: 12, bottom: 4, right: 8)
        bar.translatesAutoresizingMaskIntoConstraints = false

        let content = NSView()
        content.addSubview(machineView)
        content.addSubview(shield)
        content.addSubview(bar)
        window.contentView = content

        NSLayoutConstraint.activate([
            machineView.topAnchor.constraint(equalTo: content.topAnchor),
            machineView.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            machineView.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            machineView.bottomAnchor.constraint(equalTo: bar.topAnchor),
            shield.topAnchor.constraint(equalTo: machineView.topAnchor),
            shield.leadingAnchor.constraint(equalTo: machineView.leadingAnchor),
            shield.trailingAnchor.constraint(equalTo: machineView.trailingAnchor),
            shield.bottomAnchor.constraint(equalTo: machineView.bottomAnchor),
            bar.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            bar.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            bar.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            bar.heightAnchor.constraint(equalToConstant: barHeight),
        ])
        window.center()
        refresh()
    }

    required init?(coder: NSCoder) { nil }

    func refresh() {
        let human = vm.controller == "human"
        shield.isHidden = human
        machineView.capturesSystemKeys = human
        toggleButton.title = human ? "Release to Agent" : "Take Over"
        statusLabel.stringValue = human ? "You have control — agent input is paused" : "Agent has control — view only"
        statusLabel.textColor = human ? .systemOrange : .secondaryLabelColor
        window?.makeFirstResponder(human ? machineView : shield)
    }

    @objc func toggle() {
        onToggle?()
    }

    func windowWillClose(_ notification: Notification) {
        onClose?()
    }
}
