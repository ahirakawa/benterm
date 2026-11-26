// The probe of `cargo xtask latency`. It starts a terminal app, presses
// keys as a keyboard would, and reads the screen to tell when the result
// was shown. It is the same for every app, and nothing in the app is
// changed for it.
//
// What it reads from the screen stays in memory. No picture is written
// anywhere: the output is times and a few numbers.
//
//   probe selftest
//   probe check [--ask]
//   probe start [--env K=V]... --marker RRGGBB --marker-cells N -- COMMAND...
//   probe keys  [--env K=V]... --marker RRGGBB --marker-cells N --count N
//               [--warmup N] [--plan R,D,L,... --started-dir DIR] -- COMMAND...
//
// On standard output: `sample MILLISECONDS`, `miss`, `info KEY=VALUE`.

import AppKit
import ApplicationServices
import Carbon.HIToolbox
import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import ScreenCaptureKit

// MARK: - Pictures

/// A rectangle of pixels, from the top left.
struct PixelRect: Equatable {
    var x: Int
    var y: Int
    var width: Int
    var height: Int

    var maxX: Int { x + width }
    var maxY: Int { y + height }

    func clipped(toWidth frameWidth: Int, height frameHeight: Int) -> PixelRect {
        let left = min(max(x, 0), frameWidth)
        let top = min(max(y, 0), frameHeight)
        let right = min(max(maxX, left), frameWidth)
        let bottom = min(max(maxY, top), frameHeight)
        return PixelRect(x: left, y: top, width: right - left, height: bottom - top)
    }
}

struct Color: Equatable {
    var red: UInt8
    var green: UInt8
    var blue: UInt8
}

/// A picture of the screen: four bytes a pixel, blue first. A row may be
/// longer than its pixels.
struct Picture {
    let width: Int
    let height: Int
    let bytesPerRow: Int
    let bytes: UnsafePointer<UInt8>

    /// Whether the pixel is `color`, give or take what color management
    /// does to it on the way to the screen.
    func has(_ color: Color, x: Int, y: Int) -> Bool {
        let pixel = bytes + y * bytesPerRow + x * 4
        let tolerance = 60
        return abs(Int(pixel[0]) - Int(color.blue)) <= tolerance
            && abs(Int(pixel[1]) - Int(color.green)) <= tolerance
            && abs(Int(pixel[2]) - Int(color.red)) <= tolerance
    }

    /// The pixels of `rect`, row after row.
    func copy(_ rect: PixelRect) -> [UInt8] {
        var copy = [UInt8](repeating: 0, count: rect.width * rect.height * 4)
        copy.withUnsafeMutableBytes { target in
            for row in 0..<rect.height {
                let source = bytes + (rect.y + row) * bytesPerRow + rect.x * 4
                target.baseAddress!.advanced(by: row * rect.width * 4)
                    .copyMemory(from: source, byteCount: rect.width * 4)
            }
        }
        return copy
    }
}

/// The smallest block of the marker color that counts as the marker. A
/// cell of the smallest font is larger.
let minMarkerWidth = 12
let minMarkerHeight = 6

/// The topmost block of `color` in `area`: the cells the prompt starts
/// with. Single pixels and thin lines of the color are not it.
func findMarker(in picture: Picture, area: PixelRect, color: Color) -> PixelRect? {
    let area = area.clipped(toWidth: picture.width, height: picture.height)
    var y = area.y
    while y < area.maxY {
        var x = area.x
        while x < area.maxX {
            guard picture.has(color, x: x, y: y) else {
                x += 1
                continue
            }
            let left = x
            while x < area.maxX && picture.has(color, x: x, y: y) { x += 1 }
            let width = x - left
            guard width >= minMarkerWidth else { continue }
            let middle = left + width / 2
            var top = y
            while top > area.y && picture.has(color, x: middle, y: top - 1) { top -= 1 }
            var bottom = y + 1
            while bottom < area.maxY && picture.has(color, x: middle, y: bottom) { bottom += 1 }
            let height = bottom - top
            guard height >= minMarkerHeight else { continue }
            // A block is the color in its corners too.
            let corners = [(left + 1, top + 1), (x - 2, top + 1), (left + 1, bottom - 2), (x - 2, bottom - 2)]
            if corners.allSatisfy({ picture.has(color, x: $0.0, y: $0.1) }) {
                return PixelRect(x: left, y: top, width: width, height: height)
            }
        }
        // Every second row is enough to meet a block of the least height.
        y += 2
    }
    return nil
}

/// How many characters are typed before the line is erased.
let lineLength = 12

/// Where typed characters show: the row of the marker, from the cell
/// after the space that follows it.
struct TypedRow: Equatable {
    var rect: PixelRect
    var cellWidth: Double

    init(marker: PixelRect, markerCells: Int, frameWidth: Int, frameHeight: Int) {
        cellWidth = Double(marker.width) / Double(markerCells)
        let start = Double(marker.maxX) + cellWidth
        rect = PixelRect(
            x: Int(start.rounded()),
            y: marker.y,
            width: Int((cellWidth * Double(lineLength + 2)).rounded()),
            height: marker.height
        ).clipped(toWidth: frameWidth, height: frameHeight)
    }

    /// Whether the whole row is inside the picture. A row cut off by the
    /// edge of the screen cannot be watched.
    var isWhole: Bool {
        rect.width >= Int((cellWidth * Double(lineLength + 2)).rounded()) && rect.height >= minMarkerHeight
    }

    /// The columns of cell number `index` of the line.
    private func columns(_ index: Int) -> Range<Int> {
        let from = min(max(Int((cellWidth * Double(index)).rounded()), 0), rect.width)
        let to = min(max(Int((cellWidth * Double(index + 1)).rounded()), from), rect.width)
        return from..<to
    }

    /// How many pixels of cell `index` differ between two looks at the row.
    private func difference(_ index: Int, _ one: [UInt8], _ other: [UInt8]) -> Int {
        guard one.count == other.count, one.count == rect.width * rect.height * 4 else { return 0 }
        var changed = 0
        for row in 0..<rect.height {
            for column in columns(index) {
                let at = (row * rect.width + column) * 4
                let difference = abs(Int(one[at]) - Int(other[at]))
                    + abs(Int(one[at + 1]) - Int(other[at + 1]))
                    + abs(Int(one[at + 2]) - Int(other[at + 2]))
                if difference > 48 { changed += 1 }
            }
        }
        return changed
    }

    /// Whether two looks at the row show the same.
    func same(_ one: [UInt8], _ other: [UInt8]) -> Bool {
        (0..<(lineLength + 2)).allSatisfy { difference($0, one, other) < 4 }
    }

    /// Whether `after` shows character number `index` of the line typed
    /// since `before`. Three things have to be there: the character's
    /// cell changed, the next cell changed, because the cursor moved
    /// into it, and the character's cell is not blank. `empty` is the row
    /// with nothing typed; its last cell tells what blank looks like. A
    /// cursor that only moved, or only changed its shape, is not a
    /// character, and neither is a change elsewhere in the row.
    func showsCharacter(_ index: Int, before: [UInt8], after: [UInt8], empty: [UInt8]) -> Bool {
        guard index + 1 < lineLength + 2 else { return false }
        guard difference(index, before, after) >= 4, difference(index + 1, before, after) >= 4 else { return false }
        // The blank last cell, moved over the character's cell.
        let blank = columns(lineLength + 1)
        let cell = columns(index)
        guard after.count == empty.count, after.count == rect.width * rect.height * 4 else { return false }
        var ink = 0
        for row in 0..<rect.height {
            for offset in 0..<min(cell.count, blank.count) {
                let here = (row * rect.width + cell.lowerBound + offset) * 4
                let there = (row * rect.width + blank.lowerBound + offset) * 4
                let difference = abs(Int(after[here]) - Int(empty[there]))
                    + abs(Int(after[here + 1]) - Int(empty[there + 1]))
                    + abs(Int(after[here + 2]) - Int(empty[there + 2]))
                if difference > 48 { ink += 1 }
            }
        }
        return ink >= 4
    }
}

// MARK: - Time

var timebase: mach_timebase_info_data_t = {
    var info = mach_timebase_info_data_t()
    mach_timebase_info(&info)
    return info
}()

/// Milliseconds from one reading of the machine's clock to a later one.
func milliseconds(from start: UInt64, to end: UInt64) -> Double {
    let ticks = Double(end) - Double(start)
    return ticks * Double(timebase.numer) / Double(timebase.denom) / 1_000_000
}

// MARK: - The frames of the screen

/// The frames of the screen in the order they were shown. The capture
/// adds them and the measurement takes them, one by one, so that the
/// first frame with a change is never skipped for a later one.
final class Frames {
    private let condition = NSCondition()
    private var pending: [(CVPixelBuffer, UInt64)] = []
    private var current: (CVPixelBuffer, UInt64)?
    /// The capture reuses a few buffers. Holding more than this many
    /// would stop it.
    private let limit = 5
    /// All frames that came.
    private(set) var count = 0
    /// Frames that were let go unseen while a key's result was waited
    /// for. A time measured then may be a frame too long.
    private(set) var droppedWhileWatching = 0
    private var watching = false

    func add(_ buffer: CVPixelBuffer, shownAt time: UInt64) {
        condition.lock()
        count += 1
        pending.append((buffer, time))
        if pending.count > limit {
            pending.removeFirst()
            if watching { droppedWhileWatching += 1 }
        }
        condition.broadcast()
        condition.unlock()
    }

    /// From a key press until its result is found, frames that are let
    /// go unseen are counted.
    func watch(_ on: Bool) {
        condition.lock()
        watching = on
        condition.unlock()
    }

    func counted() -> (frames: Int, dropped: Int) {
        condition.lock()
        defer { condition.unlock() }
        return (count, droppedWhileWatching)
    }

    private func look<T>(_ frame: (CVPixelBuffer, UInt64), _ body: (Picture, UInt64) -> T?) -> T? {
        let (buffer, time) = frame
        CVPixelBufferLockBaseAddress(buffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(buffer) else { return nil }
        let picture = Picture(
            width: CVPixelBufferGetWidth(buffer),
            height: CVPixelBufferGetHeight(buffer),
            bytesPerRow: CVPixelBufferGetBytesPerRow(buffer),
            bytes: UnsafePointer(base.assumingMemoryBound(to: UInt8.self))
        )
        return body(picture, time)
    }

    /// Gives `body` every frame from now on, oldest first, until it
    /// answers or `seconds` have passed. With `fromCurrent`, the frame
    /// on the screen now is given first: a screen that does not change
    /// has no new frames.
    func wait<T>(seconds: Double, fromCurrent: Bool, _ body: (Picture, UInt64) -> T?) -> T? {
        let deadline = Date().addingTimeInterval(seconds)
        condition.lock()
        defer { condition.unlock() }
        if fromCurrent, let current, let answer = look(current, body) { return answer }
        while true {
            while !pending.isEmpty {
                let frame = pending.removeFirst()
                current = frame
                if let answer = look(frame, body) { return answer }
            }
            if !condition.wait(until: deadline) { return nil }
        }
    }

    /// Takes the frames that are waiting and looks at the newest.
    func now<T>(_ body: (Picture, UInt64) -> T?) -> T? {
        condition.lock()
        defer { condition.unlock() }
        if let last = pending.last { current = last }
        pending.removeAll()
        guard let current else { return nil }
        return look(current, body)
    }
}

final class Capture: NSObject, SCStreamOutput, SCStreamDelegate {
    let frames = Frames()
    private var stream: SCStream?
    private let queue = DispatchQueue(label: "benterm.latency.frames")
    /// Pixels for a point of the display.
    private(set) var scale = 1.0
    private(set) var origin = CGPoint.zero

    /// Starts reading the main display.
    func start() throws {
        let displayID = CGMainDisplayID()
        let found = DispatchSemaphore(value: 0)
        var content: SCShareableContent?
        var failure: Error?
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { shareable, error in
            content = shareable
            failure = error
            found.signal()
        }
        guard found.wait(timeout: .now() + 15) == .success else {
            throw Failure("macOS did not say what is on the screen: is Screen Recording allowed?")
        }
        guard let display = content?.displays.first(where: { $0.displayID == displayID }) else {
            throw Failure("the screen cannot be read: \(failure?.localizedDescription ?? "no display")")
        }
        let filter = SCContentFilter(display: display, excludingWindows: [])
        let bounds = CGDisplayBounds(displayID)
        origin = bounds.origin
        scale = Double(filter.pointPixelScale)
        let configuration = SCStreamConfiguration()
        configuration.width = Int((Double(bounds.width) * scale).rounded())
        configuration.height = Int((Double(bounds.height) * scale).rounded())
        // As often as the display shows a frame.
        configuration.minimumFrameInterval = CMTime(value: 1, timescale: 240)
        configuration.pixelFormat = kCVPixelFormatType_32BGRA
        configuration.colorSpaceName = CGColorSpace.sRGB
        configuration.showsCursor = false
        configuration.queueDepth = 8
        let stream = SCStream(filter: filter, configuration: configuration, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        let started = DispatchSemaphore(value: 0)
        stream.startCapture { error in
            failure = error
            started.signal()
        }
        guard started.wait(timeout: .now() + 15) == .success else {
            throw Failure("the screen did not start being read")
        }
        if let failure { throw Failure("the screen cannot be read: \(failure.localizedDescription)") }
        self.stream = stream
        guard frames.wait(seconds: 5, fromCurrent: true, { _, _ in true }) != nil else {
            throw Failure("the screen gave no frame")
        }
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sample.isValid,
            let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false)
                as? [[SCStreamFrameInfo: Any]],
            let info = attachments.first,
            let rawStatus = info[.status] as? Int,
            let status = SCFrameStatus(rawValue: rawStatus),
            // The first frame of a capture is `started`; it is a picture too.
            status == .complete || status == .started,
            let shownAt = info[.displayTime] as? UInt64,
            let buffer = sample.imageBuffer
        else { return }
        frames.add(buffer, shownAt: shownAt)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        fail("the screen stopped being read: \(error.localizedDescription)")
    }

    /// A rectangle of the screen, in points, as pixels of a frame.
    func pixels(_ rect: CGRect) -> PixelRect {
        PixelRect(
            x: Int(((Double(rect.minX) - Double(origin.x)) * scale).rounded()),
            y: Int(((Double(rect.minY) - Double(origin.y)) * scale).rounded()),
            width: Int((Double(rect.width) * scale).rounded()),
            height: Int((Double(rect.height) * scale).rounded())
        )
    }
}

// MARK: - Failing

struct Failure: Error {
    let message: String
    init(_ message: String) { self.message = message }
}

/// What has to be undone when the probe ends, however it ends.
var cleanup: [() -> Void] = []
let cleanupLock = NSLock()

/// Held while the steps run, so that an interrupt during a failure
/// waits for them and does not end the probe halfway.
let cleanupRunning = NSLock()

func runCleanup() {
    cleanupRunning.lock()
    defer { cleanupRunning.unlock() }
    cleanupLock.lock()
    let steps = cleanup
    cleanup = []
    cleanupLock.unlock()
    for step in steps.reversed() { step() }
}

func fail(_ message: String) -> Never {
    runCleanup()
    FileHandle.standardError.write("probe: \(message)\n".data(using: .utf8)!)
    exit(1)
}

// MARK: - Windows

/// The largest window of the process on the screen, in points from the
/// top left of the main display.
func windowFrame(of pid: pid_t) -> CGRect? {
    guard let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
        as? [[CFString: Any]]
    else { return nil }
    var largest: CGRect?
    for window in windows {
        guard (window[kCGWindowOwnerPID] as? Int).map(pid_t.init) == pid,
            window[kCGWindowLayer] as? Int == 0,
            let bounds = window[kCGWindowBounds] as? NSDictionary,
            let frame = CGRect(dictionaryRepresentation: bounds)
        else { continue }
        if frame.width * frame.height > (largest.map { $0.width * $0.height } ?? 0) { largest = frame }
    }
    return largest
}

/// The process whose window is in front of all others.
func frontProcess() -> pid_t? {
    guard let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
        as? [[CFString: Any]]
    else { return nil }
    // The list is front to back.
    let front = windows.first { $0[kCGWindowLayer] as? Int == 0 }
    return (front?[kCGWindowOwnerPID] as? Int).map(pid_t.init)
}

/// The process the keyboard's keys go to, as the accessibility API
/// tells it, or else the owner of the frontmost window.
func keyProcess() -> pid_t? {
    var value: CFTypeRef?
    let system = AXUIElementCreateSystemWide()
    if AXUIElementCopyAttributeValue(system, kAXFocusedApplicationAttribute as CFString, &value) == .success,
        let value, CFGetTypeID(value) == AXUIElementGetTypeID()
    {
        var pid: pid_t = 0
        if AXUIElementGetPid(value as! AXUIElement, &pid) == .success { return pid }
    }
    return frontProcess()
}

func bringToFront(_ pid: pid_t) throws {
    for _ in 0..<20 {
        if frontProcess() == pid && keyProcess() == pid { return }
        let app = AXUIElementCreateApplication(pid)
        AXUIElementSetAttributeValue(app, kAXFrontmostAttribute as CFString, kCFBooleanTrue)
        NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateAllWindows])
        Thread.sleep(forTimeInterval: 0.2)
    }
    throw Failure("the app's window did not come to the front")
}

/// Moves and resizes the first window of the process.
func setWindowFrame(of pid: pid_t, to frame: CGRect) throws {
    let app = AXUIElementCreateApplication(pid)
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &value) == .success,
        let windows = value as? [AXUIElement], let window = windows.first
    else { throw Failure("the app's window cannot be resized: is Accessibility allowed?") }
    var origin = frame.origin
    var size = frame.size
    guard let position = AXValueCreate(.cgPoint, &origin), let extent = AXValueCreate(.cgSize, &size) else {
        throw Failure("the app's window cannot be resized")
    }
    guard AXUIElementSetAttributeValue(window, kAXPositionAttribute as CFString, position) == .success,
        AXUIElementSetAttributeValue(window, kAXSizeAttribute as CFString, extent) == .success
    else { throw Failure("the app's window did not take its place and size") }
    // Both apps have to end up with the same frame, or they are not compared.
    for _ in 0..<30 {
        Thread.sleep(forTimeInterval: 0.1)
        if let now = windowFrame(of: pid),
            abs(now.minX - frame.minX) <= 4, abs(now.minY - frame.minY) <= 4,
            abs(now.width - frame.width) <= 4, abs(now.height - frame.height) <= 4
        {
            return
        }
    }
    let now = windowFrame(of: pid).map { "\(Int($0.width))x\(Int($0.height))" } ?? "no window"
    throw Failure("the app's window is \(now) and not \(Int(frame.width))x\(Int(frame.height)) as asked")
}

/// The frame both apps are given for the key presses: as much of the
/// main display as fits, up to 1700 by 1300 points.
func measuringFrame() -> CGRect {
    let display = CGDisplayBounds(CGMainDisplayID())
    // Below the menu bar, and above a Dock at the bottom.
    let visible = NSScreen.screens.first?.visibleFrame ?? CGRect(x: 0, y: 0, width: display.width, height: display.height - 40)
    let top = display.height - visible.maxY
    return CGRect(
        x: display.minX + visible.minX + 20,
        y: display.minY + top + 10,
        width: min(1700, visible.width - 40),
        height: min(1300, visible.height - 20)
    )
}

// MARK: - Keys

enum Key {
    static let a: CGKeyCode = 0
    static let d: CGKeyCode = 2
    static let f: CGKeyCode = 3
    static let u: CGKeyCode = 32
    static let enter: CGKeyCode = 36
    static let left: CGKeyCode = 123
}

let keySource = CGEventSource(stateID: .hidSystemState)

/// Presses and releases a key where the keyboard's keys come in, and
/// returns the time right before the press. The key goes to whatever
/// app has the keyboard, so it is pressed only while that is `pid`.
@discardableResult
func press(_ key: CGKeyCode, _ flags: CGEventFlags = [], to pid: pid_t) throws -> UInt64 {
    guard keyProcess() == pid else {
        throw Failure("another app took the keyboard: nothing more is typed")
    }
    guard let down = CGEvent(keyboardEventSource: keySource, virtualKey: key, keyDown: true),
        let up = CGEvent(keyboardEventSource: keySource, virtualKey: key, keyDown: false)
    else { fail("a key event cannot be made") }
    down.flags = flags
    up.flags = flags
    let pressed = mach_absolute_time()
    down.post(tap: .cghidEventTap)
    up.post(tap: .cghidEventTap)
    return pressed
}

/// Switches the keyboard to a layout that types ASCII, so that no input
/// method takes the keys, and returns the step that switches back.
func typeASCII() throws {
    guard let before = TISCopyCurrentKeyboardInputSource()?.takeRetainedValue() else {
        throw Failure("the keyboard layout cannot be read")
    }
    var layout: TISInputSource?
    for id in ["com.apple.keylayout.ABC", "com.apple.keylayout.US"] {
        let wanted = [kTISPropertyInputSourceID as String: id] as CFDictionary
        if let list = TISCreateInputSourceList(wanted, false)?.takeRetainedValue() as? [TISInputSource],
            let first = list.first
        {
            layout = first
            break
        }
    }
    guard let layout else {
        throw Failure("add the ABC or the U.S. keyboard layout to the input sources: the keys are sent by position")
    }
    guard TISSelectInputSource(layout) == noErr else {
        throw Failure("the keyboard layout cannot be switched")
    }
    cleanupLock.lock()
    cleanup.append { TISSelectInputSource(before) }
    cleanupLock.unlock()
}

// MARK: - The app that is measured

/// Starts the app and returns it with the time right before the start.
func startApp(_ command: [String], environment additions: [String: String]) throws -> (Process, UInt64) {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: command[0])
    process.arguments = Array(command.dropFirst())
    var environment = ProcessInfo.processInfo.environment
    for (name, value) in additions { environment[name] = value }
    process.environment = environment
    process.standardInput = FileHandle.nullDevice
    process.standardOutput = FileHandle.nullDevice
    process.standardError = FileHandle.nullDevice
    let started = mach_absolute_time()
    do {
        try process.run()
    } catch {
        throw Failure("\(command[0]) did not start: \(error.localizedDescription)")
    }
    let pid = process.processIdentifier
    cleanupLock.lock()
    cleanup.append { stopApp(pid) }
    cleanupLock.unlock()
    return (process, started)
}

/// Ends the app and waits until its window is gone.
func stopApp(_ pid: pid_t) {
    kill(pid, SIGTERM)
    for attempt in 0..<50 {
        if kill(pid, 0) != 0 && windowFrame(of: pid) == nil { return }
        if attempt == 20 { kill(pid, SIGKILL) }
        Thread.sleep(forTimeInterval: 0.1)
    }
}

/// Waits for the prompt of the shell: the marker inside the app's
/// window. Returns the marker and when the frame with it was shown.
func waitForPrompt(
    _ capture: Capture, pid: pid_t, color: Color, seconds: Double, fromCurrent: Bool,
    leftPart: Double = 1.0
) -> (PixelRect, UInt64)? {
    capture.frames.wait(seconds: seconds, fromCurrent: fromCurrent) { picture, shownAt in
        guard var window = windowFrame(of: pid) else { return nil }
        window.size.width *= leftPart
        guard let marker = findMarker(in: picture, area: capture.pixels(window), color: color) else { return nil }
        return (marker, shownAt)
    }
}

// MARK: - Load of the machine

/// The ticks the processors have spent busy and in all.
func processorTicks() -> (busy: Double, all: Double) {
    var info = host_cpu_load_info()
    var count = mach_msg_type_number_t(MemoryLayout<host_cpu_load_info>.size / MemoryLayout<integer_t>.size)
    let result = withUnsafeMutablePointer(to: &info) {
        $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
            host_statistics(mach_host_self(), HOST_CPU_LOAD_INFO, $0, &count)
        }
    }
    guard result == KERN_SUCCESS else { return (0, 0) }
    let user = Double(info.cpu_ticks.0)
    let system = Double(info.cpu_ticks.1)
    let idle = Double(info.cpu_ticks.2)
    let nice = Double(info.cpu_ticks.3)
    return (user + system + nice, user + system + idle + nice)
}

// MARK: - Arguments

struct Arguments {
    var environment: [String: String] = [:]
    var marker = Color(red: 255, green: 0, blue: 255)
    var markerCells = 4
    var count = 100
    var warmup = 5
    var plan: [String] = []
    var startedDir: String?
    var timeout = 30.0
    var command: [String] = []

    init(_ arguments: [String]) throws {
        var rest = arguments[...]
        func value(_ name: String) throws -> String {
            guard let value = rest.popFirst() else { throw Failure("\(name) needs a value") }
            return value
        }
        func number(_ name: String) throws -> Int {
            let text = try value(name)
            guard let number = Int(text), number >= 0 else { throw Failure("\(name) needs a number, not \(text)") }
            return number
        }
        while let argument = rest.popFirst() {
            switch argument {
            case "--env":
                let pair = try value(argument).split(separator: "=", maxSplits: 1).map(String.init)
                guard pair.count == 2 else { throw Failure("--env needs NAME=VALUE") }
                environment[pair[0]] = pair[1]
            case "--marker":
                let text = try value(argument)
                guard text.count == 6, let rgb = UInt32(text, radix: 16) else {
                    throw Failure("--marker needs RRGGBB")
                }
                marker = Color(red: UInt8(rgb >> 16), green: UInt8(rgb >> 8 & 0xff), blue: UInt8(rgb & 0xff))
            case "--marker-cells": markerCells = max(try number(argument), 1)
            case "--count": count = try number(argument)
            case "--warmup": warmup = try number(argument)
            case "--plan": plan = try value(argument).split(separator: ",").map(String.init)
            case "--started-dir": startedDir = try value(argument)
            case "--timeout": timeout = Double(try number(argument))
            case "--":
                command = Array(rest)
                rest = []
            default: throw Failure("unknown option \(argument)")
            }
        }
        if command.isEmpty { throw Failure("no command to start after --") }
    }
}

// MARK: - probe check

func check(ask: Bool) -> Bool {
    var screen = CGPreflightScreenCaptureAccess()
    var keys = CGPreflightPostEventAccess()
    var accessibility = AXIsProcessTrusted()
    if ask {
        // Each of these makes macOS show its dialog once and list the
        // terminal in System Settings.
        if !screen { screen = CGRequestScreenCaptureAccess() }
        if !keys { keys = CGRequestPostEventAccess() }
        if !accessibility {
            let prompt = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
            accessibility = AXIsProcessTrustedWithOptions(prompt)
        }
    }
    print("info screen_recording=\(screen)")
    print("info keys=\(keys)")
    print("info accessibility=\(accessibility)")
    return screen && keys && accessibility
}

// MARK: - probe start

/// What the thread that raises a hidden window shares with the
/// measurement.
final class Raising: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false
    private var didRaise = false

    var isDone: Bool { lock.withLock { done } }
    var wasRaised: Bool { lock.withLock { didRaise } }
    func raised() { lock.withLock { didRaise = true } }
    func finish() { lock.withLock { done = true } }
}

/// Starts the app once and tells how long its prompt took to show.
func measureStart(_ arguments: Arguments) throws {
    let capture = Capture()
    try capture.start()
    // Something of the marker's color on the screen could be taken for
    // the prompt once the app's window is over it.
    let already = capture.frames.now { picture, _ in
        findMarker(
            in: picture, area: PixelRect(x: 0, y: 0, width: picture.width, height: picture.height),
            color: arguments.marker)
    }
    if already != nil {
        throw Failure("something on the main display has the color of the marker: hide it and run again")
    }
    let (process, started) = try startApp(arguments.command, environment: arguments.environment)
    let pid = process.processIdentifier
    // A window that opens behind another one is not on the screen, and an
    // app may draw less often there. macOS puts a new app in front when
    // the terminal that started it is in front; when it does not, the
    // window is raised as soon as it is there, and the result says so.
    let raising = Raising()
    Thread.detachNewThread {
        while !raising.isDone {
            if windowFrame(of: pid) != nil, frontProcess() != pid {
                let app = AXUIElementCreateApplication(pid)
                AXUIElementSetAttributeValue(app, kAXFrontmostAttribute as CFString, kCFBooleanTrue)
                raising.raised()
            }
            Thread.sleep(forTimeInterval: 0.02)
        }
    }
    defer { raising.finish() }
    guard
        let (_, shownAt) = waitForPrompt(
            capture, pid: pid, color: arguments.marker, seconds: arguments.timeout, fromCurrent: false)
    else {
        throw Failure(
            "no prompt showed within \(Int(arguments.timeout)) seconds: the window has to open on the main display")
    }
    guard shownAt > started else { throw Failure("the frame with the prompt is older than the start") }
    print("sample \(String(format: "%.3f", milliseconds(from: started, to: shownAt)))")
    if let window = windowFrame(of: pid) {
        print("info window_points=\(Int(window.width))x\(Int(window.height))")
    }
    print("info raised_by_probe=\(raising.wasRaised)")
}

// MARK: - probe keys

func writersRunning(in directory: String) -> Int {
    let names = (try? FileManager.default.contentsOfDirectory(atPath: directory)) ?? []
    return names.compactMap { pid_t($0) }.filter { kill($0, 0) == 0 }.count
}

/// Opens the panes of the plan and starts a writer in each.
func openPanes(_ plan: [String], startedDir: String, pid: pid_t) throws {
    var writers = 0
    for step in plan {
        switch step {
        case "R", "D":
            try press(Key.d, step == "R" ? .maskCommand : [.maskCommand, .maskShift], to: pid)
            Thread.sleep(forTimeInterval: 0.5)
            // The shell of the new pane reads this when it is ready. Typed
            // into another pane it starts no writer where one is counted
            // on, and the run ends here or when the prompt is looked for.
            try press(Key.f, to: pid)
            try press(Key.enter, to: pid)
            writers += 1
            let deadline = Date().addingTimeInterval(15)
            while writersRunning(in: startedDir) < writers {
                if Date() > deadline {
                    throw Failure(
                        "pane \(writers) did not start its writer: \(writersRunning(in: startedDir)) of \(writers) run")
                }
                Thread.sleep(forTimeInterval: 0.05)
            }
        case "L":
            try press(Key.left, [.maskCommand, .maskAlternate, .maskNumericPad, .maskSecondaryFn], to: pid)
            Thread.sleep(forTimeInterval: 0.5)
        default:
            throw Failure("unknown step \(step) in the plan")
        }
    }
}

/// Starts the app, opens the panes of the plan if there is one, and
/// presses keys at the prompt of the first pane.
func measureKeys(_ arguments: Arguments) throws {
    let capture = Capture()
    try capture.start()
    try typeASCII()
    let (process, _) = try startApp(arguments.command, environment: arguments.environment)
    let pid = process.processIdentifier
    let color = arguments.marker

    // The window is put on the main display before its prompt is looked
    // for there.
    let opens = Date().addingTimeInterval(arguments.timeout)
    while windowFrame(of: pid) == nil {
        if Date() > opens { throw Failure("the app opened no window within \(Int(arguments.timeout)) seconds") }
        Thread.sleep(forTimeInterval: 0.05)
    }
    try bringToFront(pid)
    try setWindowFrame(of: pid, to: measuringFrame())
    guard waitForPrompt(capture, pid: pid, color: color, seconds: arguments.timeout, fromCurrent: true) != nil else {
        throw Failure("no prompt showed within \(Int(arguments.timeout)) seconds")
    }
    Thread.sleep(forTimeInterval: 0.5)
    try bringToFront(pid)
    guard let window = windowFrame(of: pid) else { throw Failure("the app has no window") }
    print("info window_points=\(Int(window.width))x\(Int(window.height))")
    print("info pixels_per_point=\(capture.scale)")

    var area = window
    if !arguments.plan.isEmpty {
        guard let startedDir = arguments.startedDir else { throw Failure("--plan needs --started-dir") }
        try openPanes(arguments.plan, startedDir: startedDir, pid: pid)
        // The first pane is the left half; the writers are on the right.
        area.size.width *= 0.55
        Thread.sleep(forTimeInterval: 1.0)
    }

    // The prompt, once it has stopped moving.
    var marker: PixelRect?
    for _ in 0..<20 {
        let found = capture.frames.wait(seconds: 5, fromCurrent: true) { picture, _ in
            findMarker(in: picture, area: capture.pixels(area), color: color)
        }
        Thread.sleep(forTimeInterval: 0.3)
        let again = capture.frames.now { picture, _ in
            findMarker(in: picture, area: capture.pixels(area), color: color)
        }
        if let found, found == again {
            marker = found
            break
        }
    }
    guard let marker else { throw Failure("the prompt of the first pane was not found") }
    let (frameWidth, frameHeight) = capture.frames.now { picture, _ in (picture.width, picture.height) } ?? (0, 0)
    let row = TypedRow(marker: marker, markerCells: arguments.markerCells, frameWidth: frameWidth, frameHeight: frameHeight)
    guard row.isWhole else { throw Failure("the prompt is too close to the edge of the screen") }
    print("info cell_pixels=\(String(format: "%.1f", row.cellWidth))x\(marker.height)")

    func look() throws -> [UInt8] {
        guard let row = capture.frames.now({ picture, _ in picture.copy(row.rect) }) else {
            throw Failure("the screen gave no frame")
        }
        return row
    }

    /// Waits until the row has not changed for `seconds`.
    func settle(_ seconds: Double) throws {
        var last = try look()
        let giveUp = Date().addingTimeInterval(5)
        while true {
            let changed: Bool? = capture.frames.wait(seconds: seconds, fromCurrent: false) { picture, _ in
                let now = picture.copy(row.rect)
                if now == last { return nil }
                last = now
                return true
            }
            if changed == nil { return }
            if Date() > giveUp { throw Failure("the row of the prompt keeps changing: is something over it?") }
        }
    }

    // The row with nothing typed: every line starts from it.
    try settle(0.3)
    let empty = try look()

    /// Erases the line and waits until the row is as it was with nothing
    /// typed, so that a late repaint is not taken for the next key.
    func clearLine() throws {
        try press(Key.u, .maskControl, to: pid)
        let cleared: Bool? = capture.frames.wait(seconds: 3, fromCurrent: true) { picture, _ in
            row.same(picture.copy(row.rect), empty) ? true : nil
        }
        if cleared == nil { throw Failure("the line at the prompt did not clear") }
    }

    var typed = 0
    var samples = 0
    var attempts = 0
    var missesInARow = 0
    // Enough tries for a few misses, and an end if most keys miss.
    let mostAttempts = arguments.warmup + arguments.count + max(10, arguments.count / 5)
    let loadBefore = processorTicks()
    let framesBefore = capture.frames.counted()
    let began = mach_absolute_time()
    while samples < arguments.count {
        guard attempts < mostAttempts else {
            throw Failure("only \(samples) of \(arguments.count) keys showed in \(attempts) tries")
        }
        if typed == lineLength {
            try clearLine()
            typed = 0
        }
        // A different pause every time, so that the keys do not fall in
        // step with the frames of the display.
        try settle(0.12 + Double.random(in: 0...0.08))
        let before = try look()
        capture.frames.watch(true)
        let pressed = try press(Key.a, to: pid)
        let shownAt: UInt64? = capture.frames.wait(seconds: 1, fromCurrent: false) { picture, shownAt in
            guard shownAt > pressed else { return nil }
            return row.showsCharacter(typed, before: before, after: picture.copy(row.rect), empty: empty) ? shownAt : nil
        }
        capture.frames.watch(false)
        let scored = attempts >= arguments.warmup
        attempts += 1
        if let shownAt {
            typed += 1
            missesInARow = 0
            if scored {
                samples += 1
                print("sample \(String(format: "%.3f", milliseconds(from: pressed, to: shownAt)))")
            }
        } else {
            missesInARow += 1
            if scored { print("miss") }
            if missesInARow >= 5 {
                throw Failure("five keys in a row did not show at the prompt: is the first pane focused?")
            }
            // Where the line is now is not known: start it again.
            try clearLine()
            typed = 0
        }
    }
    let seconds = milliseconds(from: began, to: mach_absolute_time()) / 1000
    let loadAfter = processorTicks()
    let framesAfter = capture.frames.counted()
    if loadAfter.all > loadBefore.all {
        let busy = (loadAfter.busy - loadBefore.busy) / (loadAfter.all - loadBefore.all) * 100
        print("info cpu_busy_percent=\(String(format: "%.1f", busy))")
    }
    if let mode = CGDisplayCopyDisplayMode(CGMainDisplayID()) {
        let refresh = mode.refreshRate > 0 ? mode.refreshRate : Double(NSScreen.screens.first?.maximumFramesPerSecond ?? 0)
        print("info display_hz=\(String(format: "%.0f", refresh))")
    }
    // How often the screen changed while the keys were pressed: close to
    // display_hz beside busy panes, and far below it in a quiet window.
    let framesPerSecond = Double(framesAfter.frames - framesBefore.frames) / max(seconds, 0.001)
    print("info frames_per_second=\(String(format: "%.0f", framesPerSecond))")
    print("info frames_dropped=\(framesAfter.dropped - framesBefore.dropped)")
    if let startedDir = arguments.startedDir, !arguments.plan.isEmpty {
        let writers = arguments.plan.filter { $0 != "L" }.count
        let running = writersRunning(in: startedDir)
        print("info writers_running=\(running)")
        guard running == writers else { throw Failure("only \(running) of \(writers) writers were still running at the end") }
    }
}

// MARK: - probe selftest

func selftest() {
    var failures: [String] = []
    func expect(_ condition: Bool, _ what: String) {
        if !condition { failures.append(what) }
    }
    let magenta = Color(red: 255, green: 0, blue: 255)
    let width = 240
    let height = 80
    // Rows are longer than their pixels, as the rows of a real frame are.
    let bytesPerRow = width * 4 + 24

    func canvas(_ draw: (inout [UInt8]) -> Void) -> [UInt8] {
        var bytes = [UInt8](repeating: 0x20, count: bytesPerRow * height)
        draw(&bytes)
        return bytes
    }
    func fill(_ bytes: inout [UInt8], _ rect: PixelRect, _ color: Color) {
        for y in rect.y..<rect.maxY {
            for x in rect.x..<rect.maxX {
                let at = y * bytesPerRow + x * 4
                bytes[at] = color.blue
                bytes[at + 1] = color.green
                bytes[at + 2] = color.red
                bytes[at + 3] = 255
            }
        }
    }
    func with<T>(_ bytes: [UInt8], _ body: (Picture) -> T) -> T {
        bytes.withUnsafeBufferPointer {
            body(Picture(width: width, height: height, bytesPerRow: bytesPerRow, bytes: $0.baseAddress!))
        }
    }
    let whole = PixelRect(x: 0, y: 0, width: width, height: height)
    let block = PixelRect(x: 20, y: 31, width: 32, height: 16)

    // The marker is found where it is, to the pixel.
    let plain = canvas { fill(&$0, block, magenta) }
    expect(with(plain) { findMarker(in: $0, area: whole, color: magenta) } == block, "the marker is found")

    // A color that color management moved a little is still the marker.
    let shifted = canvas { fill(&$0, block, Color(red: 234, green: 51, blue: 247)) }
    expect(with(shifted) { findMarker(in: $0, area: whole, color: magenta) } == block, "a shifted color is found")

    // Pink that is not magenta is not.
    let pink = canvas { fill(&$0, block, Color(red: 255, green: 128, blue: 255)) }
    expect(with(pink) { findMarker(in: $0, area: whole, color: magenta) } == nil, "pink is not the marker")

    // Thin lines and dots of the color above it are passed over.
    let noisy = canvas {
        fill(&$0, PixelRect(x: 5, y: 4, width: 100, height: 2), magenta)
        fill(&$0, PixelRect(x: 150, y: 10, width: 5, height: 30), magenta)
        fill(&$0, PixelRect(x: 200, y: 20, width: 1, height: 1), magenta)
        fill(&$0, block, magenta)
    }
    expect(with(noisy) { findMarker(in: $0, area: whole, color: magenta) } == block, "lines and dots are passed over")

    // A marker outside the area that is searched is not found.
    let left = PixelRect(x: 0, y: 0, width: 18, height: height)
    expect(with(plain) { findMarker(in: $0, area: left, color: magenta) } == nil, "the area bounds the search")
    expect(
        with(canvas { _ in }) { findMarker(in: $0, area: whole, color: magenta) } == nil, "no marker, nothing found")

    // An area that reaches past the picture is cut to it.
    let beyond = PixelRect(x: -50, y: -50, width: 1000, height: 1000)
    expect(with(plain) { findMarker(in: $0, area: beyond, color: magenta) } == block, "the area is cut to the picture")

    // Typed characters show from the cell after the marker's space.
    let row = TypedRow(marker: block, markerCells: 4, frameWidth: width, frameHeight: height)
    expect(row.cellWidth == 8, "a cell is a fourth of the marker")
    expect(row.rect == PixelRect(x: 60, y: 31, width: 112, height: 16), "the row starts after the space: \(row.rect)")

    let white = Color(red: 220, green: 220, blue: 220)
    func cell(_ index: Int) -> PixelRect { PixelRect(x: 60 + index * 8, y: 31, width: 8, height: 16) }
    func glyph(_ index: Int) -> PixelRect { PixelRect(x: 62 + index * 8, y: 36, width: 4, height: 8) }
    // Before: the cursor in cell 0. After: a letter there, the cursor in cell 1.
    let empty = canvas {
        fill(&$0, block, magenta)
        fill(&$0, cell(0), white)
    }
    let one = canvas {
        fill(&$0, block, magenta)
        fill(&$0, glyph(0), white)
        fill(&$0, cell(1), white)
    }
    let two = canvas {
        fill(&$0, block, magenta)
        fill(&$0, glyph(0), white)
        fill(&$0, glyph(1), white)
        fill(&$0, cell(2), white)
    }
    let emptyRow = with(empty) { $0.copy(row.rect) }
    let oneRow = with(one) { $0.copy(row.rect) }
    let twoRow = with(two) { $0.copy(row.rect) }
    expect(emptyRow.count == 112 * 16 * 4, "the row is copied without the rest of its lines")
    expect(row.isWhole, "the row is whole")
    expect(row.showsCharacter(0, before: emptyRow, after: oneRow, empty: emptyRow), "the first character shows")
    expect(row.showsCharacter(1, before: oneRow, after: twoRow, empty: emptyRow), "the second character shows")
    expect(!row.showsCharacter(0, before: emptyRow, after: emptyRow, empty: emptyRow), "no change, no character")
    // A change further along the row is not this key.
    expect(
        !row.showsCharacter(5, before: emptyRow, after: oneRow, empty: emptyRow),
        "a change in another cell is not this key")
    // A cursor that moved on without a character: the cell it left is blank.
    let moved = canvas {
        fill(&$0, block, magenta)
        fill(&$0, cell(1), white)
    }
    let movedRow = with(moved) { $0.copy(row.rect) }
    expect(
        !row.showsCharacter(0, before: emptyRow, after: movedRow, empty: emptyRow),
        "a cursor that only moved is not a character")
    // A cursor that changed its shape where it stands, as when the window
    // loses the keyboard: the next cell did not change.
    let hollow = canvas {
        fill(&$0, block, magenta)
        fill(&$0, cell(0), white)
        fill(&$0, PixelRect(x: 61, y: 32, width: 6, height: 14), Color(red: 0x20, green: 0x20, blue: 0x20))
    }
    let hollowRow = with(hollow) { $0.copy(row.rect) }
    expect(
        !row.showsCharacter(0, before: emptyRow, after: hollowRow, empty: emptyRow),
        "a cursor that changed its shape is not a character")
    // A line that is erased: the row is as it was with nothing typed.
    expect(row.same(emptyRow, with(empty) { $0.copy(row.rect) }), "an erased line is the empty row")
    expect(!row.same(emptyRow, oneRow), "a line with a character is not the empty row")
    expect(
        !row.showsCharacter(1, before: twoRow, after: emptyRow, empty: emptyRow),
        "an erased line is not the next character")
    // A row the edge of the screen cuts off is not watched.
    let cut = TypedRow(marker: block, markerCells: 4, frameWidth: 100, frameHeight: height)
    expect(!cut.isWhole, "a row cut off by the edge is not whole")
    expect(
        !cut.showsCharacter(8, before: [], after: [], empty: []), "a cell past the edge shows nothing and does not trap")
    // A change outside the row, as a busy pane beside it makes, is not seen at all.
    let busy = canvas {
        fill(&$0, block, magenta)
        fill(&$0, cell(0), white)
        fill(&$0, PixelRect(x: 180, y: 0, width: 60, height: 80), white)
        fill(&$0, PixelRect(x: 0, y: 50, width: 240, height: 30), white)
    }
    expect(with(busy) { $0.copy(row.rect) } == emptyRow, "what moves beside the row is not in it")

    // Time: the ticks of the clock in milliseconds.
    let second = UInt64(1_000_000_000) * UInt64(timebase.denom) / UInt64(timebase.numer)
    expect(abs(milliseconds(from: 1000, to: 1000 + second) - 1000) < 0.001, "a second is 1000 milliseconds")

    // Frames are given oldest first: the time of a change is the time of
    // the first frame that has it, also when a later frame is waiting.
    func buffer(_ bytes: [UInt8]) -> CVPixelBuffer {
        var made: CVPixelBuffer?
        CVPixelBufferCreate(kCFAllocatorDefault, width, height, kCVPixelFormatType_32BGRA, nil, &made)
        let buffer = made!
        CVPixelBufferLockBaseAddress(buffer, [])
        let base = CVPixelBufferGetBaseAddress(buffer)!.assumingMemoryBound(to: UInt8.self)
        let stride = CVPixelBufferGetBytesPerRow(buffer)
        for y in 0..<height {
            for x in 0..<(width * 4) { base[y * stride + x] = bytes[y * bytesPerRow + x] }
        }
        CVPixelBufferUnlockBaseAddress(buffer, [])
        return buffer
    }
    let frames = Frames()
    frames.add(buffer(empty), shownAt: 100)
    let before = frames.now { picture, _ in picture.copy(row.rect) }
    expect(before == emptyRow, "the newest frame is looked at")
    frames.add(buffer(empty), shownAt: 200)
    frames.add(buffer(one), shownAt: 300)
    frames.add(buffer(two), shownAt: 400)
    let shownAt: UInt64? = frames.wait(seconds: 0.1, fromCurrent: false) { picture, shownAt in
        row.showsCharacter(0, before: emptyRow, after: picture.copy(row.rect), empty: emptyRow) ? shownAt : nil
    }
    expect(shownAt == 300, "the first frame with the change gives the time, not \(String(describing: shownAt))")
    let none: UInt64? = frames.wait(seconds: 0.05, fromCurrent: false) { _, shownAt in shownAt == 999 ? shownAt : nil }
    expect(none == nil, "waiting ends when the time is up")
    let current: UInt64? = frames.wait(seconds: 0.05, fromCurrent: true) { _, shownAt in shownAt }
    expect(current == 400, "a screen that stands still is looked at as it is")
    // Frames that are let go unseen while a key is watched are counted.
    frames.watch(true)
    for time in 500..<510 { frames.add(buffer(empty), shownAt: UInt64(time)) }
    frames.watch(false)
    expect(frames.counted().frames == 14, "every frame is counted: \(frames.counted().frames)")
    expect(frames.counted().dropped == 5, "five of ten frames did not fit: \(frames.counted().dropped)")

    // Arguments.
    if let arguments = try? Arguments([
        "--env", "A=b=c", "--marker", "ff00fe", "--marker-cells", "4", "--count", "7", "--plan", "R,D,L",
        "--started-dir", "/tmp/x", "--", "/bin/app", "--flag",
    ]) {
        expect(arguments.environment == ["A": "b=c"], "an environment value keeps its equals signs")
        expect(arguments.marker == Color(red: 255, green: 0, blue: 254), "the marker color is read")
        expect(arguments.count == 7 && arguments.plan == ["R", "D", "L"], "count and plan are read")
        expect(arguments.command == ["/bin/app", "--flag"], "the command follows --")
    } else {
        expect(false, "the arguments are read")
    }
    expect((try? Arguments(["--count", "3"])) == nil, "a command is needed")

    if failures.isEmpty {
        print("selftest ok")
    } else {
        for failure in failures { FileHandle.standardError.write("selftest: \(failure)\n".data(using: .utf8)!) }
        exit(1)
    }
}

// MARK: - main

setvbuf(stdout, nil, _IOLBF, 0)
// An interrupted run still ends the app it started.
signal(SIGINT, SIG_IGN)
signal(SIGTERM, SIG_IGN)
let interrupts = [SIGINT, SIGTERM].map { number -> DispatchSourceSignal in
    let source = DispatchSource.makeSignalSource(signal: number, queue: .global())
    source.setEventHandler {
        runCleanup()
        exit(130)
    }
    source.resume()
    return source
}

let commandLine = Array(CommandLine.arguments.dropFirst())
do {
    switch commandLine.first {
    case "selftest":
        selftest()
    case "check":
        exit(check(ask: commandLine.contains("--ask")) ? 0 : 3)
    case "start":
        try measureStart(try Arguments(Array(commandLine.dropFirst())))
    case "keys":
        try measureKeys(try Arguments(Array(commandLine.dropFirst())))
    default:
        throw Failure("usage: probe selftest | check [--ask] | start ... | keys ...")
    }
    runCleanup()
} catch let failure as Failure {
    fail(failure.message)
} catch {
    fail(error.localizedDescription)
}
