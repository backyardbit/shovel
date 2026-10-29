import AppKit

if CommandLine.arguments.contains("--encode") {
    exit(VideoEncoder.run(CommandLine.arguments))
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
