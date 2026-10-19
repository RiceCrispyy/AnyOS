import Foundation
import XCTest

@testable import LauncherCore

final class LauncherTests: XCTestCase {
    private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory
            .appendingPathComponent("anyos-launcher-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    @discardableResult
    private func write(
        _ name: String,
        contents: String = "",
        executable: Bool = false
    ) throws -> URL {
        let url = root.appendingPathComponent(name)
        try Data(contents.utf8).write(to: url)
        if executable {
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o755],
                ofItemAtPath: url.path
            )
        }
        return url
    }

    func testFindsAnyOSBinaryBesideLauncher() throws {
        let anyOS = try write("anyos", executable: true)
        let launcher = try write("AnyOSLauncher", executable: true)

        XCTAssertEqual(
            Launcher.anyOSBinary(beside: launcher)?.resolvingSymlinksInPath(),
            anyOS.resolvingSymlinksInPath()
        )
    }

    func testMissingAnyOSBinaryIsReportedNotGuessed() throws {
        let launcher = try write("AnyOSLauncher", executable: true)
        XCTAssertNil(Launcher.anyOSBinary(beside: launcher))
    }

    func testNonExecutableFileIsNotTreatedAsAnyOS() throws {
        let launcher = try write("AnyOSLauncher", executable: true)
        try write("anyos", executable: false)
        XCTAssertNil(Launcher.anyOSBinary(beside: launcher))
    }

    func testBundledProgramIsFoundUnderResources() throws {
        let binary = root.appendingPathComponent("Resources/binary")
        try FileManager.default.createDirectory(at: binary, withIntermediateDirectories: true)
        try write("Resources/binary/hello_console.exe", contents: "MZ")

        let found = Launcher.bundledProgram(
            named: "hello_console.exe",
            resources: root.appendingPathComponent("Resources")
        )
        XCTAssertEqual(found?.lastPathComponent, "hello_console.exe")
        XCTAssertNil(
            Launcher.bundledProgram(
                named: "absent.exe",
                resources: root.appendingPathComponent("Resources")
            )
        )
    }

    func testPlanRunsGuestThroughTheRunSubcommand() {
        let anyOS = URL(fileURLWithPath: "/tmp/app/Contents/MacOS/anyos")
        let program = URL(fileURLWithPath: "/tmp/guest/hello_console.exe")

        let plan = Launcher.plan(
            anyOS: anyOS,
            program: program,
            guestArguments: ["--flag", "value"]
        )

        XCTAssertEqual(plan.executable, anyOS)
        XCTAssertEqual(
            plan.arguments,
            ["run", "/tmp/guest/hello_console.exe", "--flag", "value"]
        )
    }

    func testSplitArgumentsKeepsGuestFlagsWithTheGuest() {
        let split = Launcher.splitArguments(["hello_console.exe", "-v", "--trace"])
        XCTAssertEqual(split?.program, "hello_console.exe")
        XCTAssertEqual(split?.guestArguments, ["-v", "--trace"])
    }

    func testSplitArgumentsDropsLauncherFlagsBeforeTheProgram() {
        let split = Launcher.splitArguments(["--verbose", "hello_console.exe", "-v"])
        XCTAssertEqual(split?.program, "hello_console.exe")
        XCTAssertEqual(split?.guestArguments, ["-v"])
    }

    func testSplitArgumentsWithoutAProgramIsVisualMode() {
        XCTAssertNil(Launcher.splitArguments([]))
        XCTAssertNil(Launcher.splitArguments(["--verbose"]))
    }
}
