import AppKit
import Foundation

// Match the original bubbles-emoji branding using macOS's own color emoji artwork.
// iconutil consumes this standard iconset; no image-conversion dependencies needed.
let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = points * scale
        guard let bitmap = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
        ), let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
            fatalError("Could not create icon bitmap")
        }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        let size = CGFloat(pixels)
        let font = NSFont(name: "Apple Color Emoji", size: size * 0.78)
            ?? NSFont.systemFont(ofSize: size * 0.78)
        let text = NSAttributedString(string: "🫧", attributes: [.font: font])
        let bounds = text.size()
        text.draw(at: NSPoint(x: (size - bounds.width) / 2, y: (size - bounds.height) / 2))
        context.flushGraphics()
        NSGraphicsContext.restoreGraphicsState()
        guard let data = bitmap.representation(using: .png, properties: [:]) else {
            fatalError("Could not encode icon PNG")
        }
        let suffix = scale == 2 ? "@2x" : ""
        try data.write(to: directory.appendingPathComponent("icon_\(points)x\(points)\(suffix).png"))
    }
}
