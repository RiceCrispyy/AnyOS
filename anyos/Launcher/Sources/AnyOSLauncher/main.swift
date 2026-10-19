import AppKit
import Foundation
import LauncherCore

// The launcher ships inside AnyOSLauncher.app, next to the `anyos` binary it
// runs. `swift build` produces the executable; `scripts/bundle_launcher.py`
// assembles the surrounding .app.

private func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("AnyOSLauncher: \(message)\n".utf8))
    exit(2)
}

/// Visual launch mode: no program on the command line, so ask for one.
private func chooseProgram() -> URL? {
    let app = NSApplication.shared
    app.setActivationPolicy(.regular)
    app.activate(ignoringOtherApps: true)

    let panel = NSOpenPanel()
    panel.title = "Choose a program to run in AnyOS"
    panel.allowsMultipleSelection = false
    panel.canChooseDirectories = false
    panel.canChooseFiles = true
    guard panel.runModal() == .OK else { return nil }
    return panel.url
}

let launcher = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath()
guard let anyOS = Launcher.anyOSBinary(beside: launcher) else {
    fail("no bundled anyos binary next to \(launcher.path); run scripts/bundle_launcher.py first")
}

// Contents/Resources, next to Contents/MacOS.
let resources = launcher
    .deletingLastPathComponent()
    .deletingLastPathComponent()
    .appendingPathComponent("Resources")

var guestArguments: [String] = []
var program: URL?

if let split = Launcher.splitArguments(Array(CommandLine.arguments.dropFirst())) {
    guestArguments = split.guestArguments
    let direct = URL(fileURLWithPath: split.program)
    if FileManager.default.isReadableFile(atPath: direct.path) {
        program = direct
    } else {
        program = Launcher.bundledProgram(named: split.program, resources: resources)
            ?? Launcher.bundledProgram(named: split.program + ".exe", resources: resources)
    }
    if program == nil {
        fail("no such program: \(split.program)")
    }
} else {
    // Visual mode: pick the guest program from the fixtures shipped in the app.
    program = chooseProgram()
}

guard let chosen = program else {
    exit(0)
}

let plan = Launcher.plan(anyOS: anyOS, program: chosen, guestArguments: guestArguments)
let process = Process()
process.executableURL = plan.executable
process.arguments = plan.arguments
do {
    try process.run()
} catch {
    fail("cannot start \(plan.executable.path): \(error)")
}
process.waitUntilExit()
exit(process.terminationStatus)
