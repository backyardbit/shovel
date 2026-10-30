import AppKit

enum StatusIcon {
    static func shovel() -> NSImage {
        let image = NSImage(size: NSSize(width: 18, height: 18), flipped: false) { rect in
            let transform = NSAffineTransform()
            transform.translateX(by: rect.midX, yBy: rect.midY)
            transform.rotate(byDegrees: -45)
            transform.concat()
            NSColor.black.set()

            let blade = NSBezierPath()
            blade.move(to: NSPoint(x: -3.5, y: -2.5))
            blade.line(to: NSPoint(x: -3.5, y: -6.5))
            blade.curve(to: NSPoint(x: 0, y: -10), controlPoint1: NSPoint(x: -3.5, y: -8.5), controlPoint2: NSPoint(x: -1.5, y: -10))
            blade.curve(to: NSPoint(x: 3.5, y: -6.5), controlPoint1: NSPoint(x: 1.5, y: -10), controlPoint2: NSPoint(x: 3.5, y: -8.5))
            blade.line(to: NSPoint(x: 3.5, y: -2.5))
            blade.close()
            blade.fill()

            NSBezierPath(rect: NSRect(x: -0.9, y: -3, width: 1.8, height: 9.2)).fill()

            let grip = NSBezierPath(roundedRect: NSRect(x: -2.4, y: 6, width: 4.8, height: 3.2), xRadius: 1.2, yRadius: 1.2)
            grip.lineWidth = 1.4
            grip.stroke()
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = "Shovel"
        return image
    }
}
