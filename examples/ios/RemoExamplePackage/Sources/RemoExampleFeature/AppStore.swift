import SQLite3
import SwiftUI

// MARK: - Models

struct LogEntry: Identifiable {
    let id = UUID()
    let timestamp: Date
    let capability: String
    let params: String
    let result: String
}

// MARK: - App Store

@Observable
public final class AppStore: @unchecked Sendable {
    public var counter: Int = 0
    public var username: String = "Guest"
    public var items: [String] = [
        "Morning Standup", "Design Review", "Sprint Planning", "API Integration",
        "Code Review", "Remo Demo", "Release Notes", "User Testing",
        "Launch Prep", "Post-mortem", "Architecture Review", "Performance Audit",
        "Accessibility Pass", "Localization Check", "Security Review", "Dependency Update",
        "Changelog Draft", "Beta Feedback", "Stakeholder Sync", "Ship It",
    ]
    public var currentRoute: String = "home"

    var accentColorName: String = "blue"
    var toastMessage: String?
    var showConfetti: Bool = false
    var activityLog: [LogEntry] = []

    public init() {
        seedDemoData()
    }

    var accentColor: Color {
        switch accentColorName {
        case "red": .red
        case "green": .green
        case "orange": .orange
        case "purple": .purple
        case "pink": .pink
        case "yellow": .yellow
        case "mint": .mint
        case "teal": .teal
        default: .blue
        }
    }

    func log(capability: String, params: String, result: String) {
        let entry = LogEntry(
            timestamp: .now,
            capability: capability,
            params: params,
            result: result
        )
        DispatchQueue.main.async { [self] in
            activityLog.insert(entry, at: 0)
            if activityLog.count > 200 {
                activityLog = Array(activityLog.prefix(200))
            }
        }
    }
}

// MARK: - Demo data seeding
//
// AppStore's own state is entirely in-memory, so the generic
// `userDefaults.list`/`filesystem.list`/`sqlite.query` capabilities have
// nothing real to show against a fresh install. This seeds a small,
// representative footprint in each of those three storage domains once, so
// the capabilities have something to inspect without requiring any app
// interaction first.
extension AppStore {
    fileprivate func seedDemoData() {
        seedUserDefaults()
        seedSandboxFiles()
        seedSQLiteDatabase()
    }

    private func seedUserDefaults() {
        let defaults = UserDefaults.standard
        guard defaults.object(forKey: "remo.demo.hasSeeded") == nil else { return }
        defaults.set("Guest", forKey: "username")
        defaults.set(true, forKey: "hasCompletedOnboarding")
        defaults.set(12, forKey: "launchCount")
        defaults.set("blue", forKey: "preferredAccentColor")
        defaults.set(["remo", "cdp", "swiftui"], forKey: "recentSearches")
        defaults.set(Date().timeIntervalSince1970, forKey: "lastSyncTimestamp")
        defaults.set(true, forKey: "remo.demo.hasSeeded")
    }

    private func seedSandboxFiles() {
        let fileManager = FileManager.default
        guard let documents = fileManager.urls(for: .documentDirectory, in: .userDomainMask).first else {
            return
        }
        let cacheDirectory = documents.appendingPathComponent("cache", isDirectory: true)
        try? fileManager.createDirectory(at: cacheDirectory, withIntermediateDirectories: true)

        let notes = documents.appendingPathComponent("notes.txt")
        if !fileManager.fileExists(atPath: notes.path) {
            try? "Remember to check the Remo demo before the review.".write(
                to: notes, atomically: true, encoding: .utf8)
        }

        let sessionLog = documents.appendingPathComponent("session.log")
        if !fileManager.fileExists(atPath: sessionLog.path) {
            let lines = (1...5).map { "2026-08-02 09:0\($0):00 [info] app session tick #\($0)" }
                .joined(separator: "\n")
            try? lines.write(to: sessionLog, atomically: true, encoding: .utf8)
        }

        let thumbnail = cacheDirectory.appendingPathComponent("thumbnail.dat")
        if !fileManager.fileExists(atPath: thumbnail.path) {
            try? Data((0..<256).map { UInt8($0 & 0xFF) }).write(to: thumbnail)
        }
    }

    private func seedSQLiteDatabase() {
        guard
            let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)
                .first
        else {
            return
        }
        let dbURL = documents.appendingPathComponent("demo.sqlite")

        var db: OpaquePointer?
        guard sqlite3_open(dbURL.path, &db) == SQLITE_OK, let db else { return }
        defer { sqlite3_close(db) }

        sqlite3_exec(
            db,
            """
            CREATE TABLE IF NOT EXISTS todos (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                done INTEGER NOT NULL DEFAULT 0,
                created_at REAL NOT NULL
            )
            """,
            nil, nil, nil
        )

        var countStatement: OpaquePointer?
        var existingRowCount: Int32 = 0
        if sqlite3_prepare_v2(db, "SELECT COUNT(*) FROM todos", -1, &countStatement, nil) == SQLITE_OK,
            sqlite3_step(countStatement) == SQLITE_ROW
        {
            existingRowCount = sqlite3_column_int(countStatement, 0)
        }
        sqlite3_finalize(countStatement)
        guard existingRowCount == 0 else { return }

        let sampleTodos = ["Ship Remo demo", "Write release notes", "Review PR #42", "Update onboarding copy"]
        for (index, title) in sampleTodos.enumerated() {
            var insertStatement: OpaquePointer?
            guard
                sqlite3_prepare_v2(
                    db, "INSERT INTO todos (title, done, created_at) VALUES (?, ?, ?)", -1,
                    &insertStatement, nil) == SQLITE_OK
            else { continue }
            defer { sqlite3_finalize(insertStatement) }
            // SQLITE_TRANSIENT tells SQLite to copy the string immediately —
            // `title` doesn't outlive this loop iteration otherwise.
            let sqliteTransient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
            sqlite3_bind_text(insertStatement, 1, title, -1, sqliteTransient)
            sqlite3_bind_int(insertStatement, 2, index % 2 == 0 ? 1 : 0)
            sqlite3_bind_double(insertStatement, 3, Date().timeIntervalSince1970)
            sqlite3_step(insertStatement)
        }
    }
}
