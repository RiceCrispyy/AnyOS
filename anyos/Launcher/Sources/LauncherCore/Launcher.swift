import Foundation

/// How a guest program is handed to the bundled `anyos` binary.
public struct LaunchPlan: Equatable {
    public let executable: URL
    public let arguments: [String]

    public init(executable: URL, arguments: [String]) {
        self.executable = executable
        self.arguments = arguments
    }
}

/// Locating and invoking the pieces of the app bundle.
///
/// Everything here is pure filesystem work so it can be covered by
/// `swift test` on a machine with no window server and no app bundle.
public enum Launcher {
    /// The `anyos` CLI that sits next to the launcher in `Contents/MacOS`.
    public static func anyOSBinary(
        beside launcher: URL,
        fileManager: FileManager = .default
    ) -> URL? {
        let candidate = launcher
            .deletingLastPathComponent()
            .appendingPathComponent("anyos")
        guard fileManager.isExecutableFile(atPath: candidate.path) else { return nil }
        return candidate
    }

    /// A guest program shipped in `Contents/Resources/binary`.
    public static func bundledProgram(
        named name: String,
        resources: URL,
        fileManager: FileManager = .default
    ) -> URL? {
        let candidate = resources
            .appendingPathComponent("binary")
            .appendingPathComponent(name)
        guard fileManager.isReadableFile(atPath: candidate.path) else { return nil }
        return candidate
    }

    /// `anyos run <program> [guest arguments...]`
    public static func plan(
        anyOS: URL,
        program: URL,
        guestArguments: [String] = []
    ) -> LaunchPlan {
        LaunchPlan(
            executable: anyOS,
            arguments: ["run", program.path] + guestArguments
        )
    }

    /// Split the launcher's own argv into the guest program and its arguments.
    ///
    /// The first token that is not a flag is the program; everything after it,
    /// flags included, belongs to the guest. Leading flags stay with launcher
    /// and are dropped here.
    public static func splitArguments(
        _ argv: [String]
    ) -> (program: String, guestArguments: [String])? {
        guard let index = argv.firstIndex(where: { !$0.hasPrefix("-") }) else {
            return nil
        }
        return (argv[index], Array(argv[(index + 1)...]))
    }
}
