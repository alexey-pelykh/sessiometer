// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT
//
// THROWAWAY SPIKE CODE (issue #761) — not part of the app, not a suite, never built by CI's
// `swift` job (`project.yml` carries no UI-test target; see ../README.md for the re-run recipe).
//
// These probes are DIAGNOSTIC, not aspirational. Two earlier rounds each shipped a probe bug that
// looked like a finding about the app, so every assertion here is deliberately the weakest one that
// still answers the issue's question, and every probe dumps what it actually saw:
//
//   round 1 — asserted `app.state == .runningForeground`; an LSUIElement accessory app is CORRECTLY
//             `.runningBackground`, so the probe was wrong, not the app.
//   round 2 — queried the panel header by `label`; SwiftUI publishes a combined Text element's
//             string as `value`. The header WAS in the tree the whole time and the probe missed it.
//
// Round 3 therefore asks each question through SEVERAL query strategies and reports which ones
// resolve, rather than betting the verdict on one predicate.

import XCTest

final class StatusItemUISpike: XCTestCase {
    private func note(_ s: String) {
        FileHandle.standardError.write("SPIKE761 \(s)\n".data(using: .utf8)!)
    }

    /// Launch the agent app. Never asserts `.runningForeground`: `.runningBackground` (3) is the
    /// CORRECT state for an `LSUIElement` accessory app.
    private func launch(_ label: String) -> XCUIApplication {
        let app = XCUIApplication()
        app.launch()
        note("\(label) state=\(app.state.rawValue) (3=runningBackground is correct for LSUIElement)")
        return app
    }

    /// Click the status item and wait for the panel's `Dialog` to appear.
    private func openPanel(_ app: XCUIApplication, _ label: String) -> Bool {
        let item = app.statusItems.firstMatch
        guard item.waitForExistence(timeout: 15) else {
            note("\(label) NO STATUS ITEM to click")
            return false
        }
        item.click()
        let appeared = app.dialogs.firstMatch.waitForExistence(timeout: 15)
        note("\(label) panelOpened=\(appeared)")
        return appeared
    }

    /// Probe A — does the LSUIElement app launch under XCUITest at all?
    func testLaunchAgentApp() throws {
        let app = launch("A.launch")
        defer { app.terminate() }
        XCTAssertNotEqual(app.state, .notRunning, "app failed to launch at all")
    }

    /// Probe B — is the app's OWN status item reachable and correctly labelled?
    func testOwnAppStatusItemVisible() throws {
        let app = launch("B.ownApp")
        defer { app.terminate() }

        let items = app.statusItems
        let appeared = items.firstMatch.waitForExistence(timeout: 15)
        note("B.ownApp statusItems.count=\(items.count) appeared=\(appeared)")
        if appeared { note("B.ownApp statusItem.label=\(items.firstMatch.label.debugDescription)") }
        XCTAssertTrue(appeared, "own-app statusItems query found nothing in 15s")
    }

    /// Probe C — reachable via SystemUIServer, the path issue #761 predicted? (Measured 0/20 — it is
    /// not. Round 1 measured nothing at all: its harness could not fail. See ../README.md.)
    func testSystemUIServerStatusItemVisible() throws {
        let app = launch("C.suis")
        defer { app.terminate() }

        let suis = XCUIApplication(bundleIdentifier: "com.apple.systemuiserver")
        let appeared = suis.statusItems.firstMatch.waitForExistence(timeout: 10)
        note("C.suis statusItems.count=\(suis.statusItems.count) appeared=\(appeared)")
        XCTAssertTrue(appeared, "SystemUIServer statusItems query found nothing in 10s")
    }

    /// Probe D — THE spike question: click the item, assert ONE element of the panel.
    ///
    /// Round 2 answered "no" off a `label`-based predicate. SwiftUI publishes a combined `Text`
    /// element's string under `value`, so this asks four ways and reports each independently.
    func testOpenPanelAndAssertOneElement() throws {
        let app = launch("D.panel")
        defer { app.terminate() }
        guard openPanel(app, "D.panel") else { return XCTFail("panel never opened") }

        let byValue = app.descendants(matching: .any)
            .matching(NSPredicate(format: "value BEGINSWITH 'Sessiometer.'")).firstMatch
        let byLabel = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH 'Sessiometer.'")).firstMatch

        let valueHit = byValue.waitForExistence(timeout: 10)
        let labelHit = byLabel.exists
        let anyStaticText = app.staticTexts.firstMatch.exists

        note("D.panel strategies: byVALUE=\(valueHit) byLABEL=\(labelHit) " +
             "anyStaticText=\(anyStaticText) staticTexts=\(app.staticTexts.count) " +
             "buttons=\(app.buttons.count) groups=\(app.groups.count) dialogs=\(app.dialogs.count)")
        note("D.panel FULL TREE >>>\n\(app.debugDescription)\n<<< END TREE")

        // The issue's literal question: can we assert ONE element of the panel?
        XCTAssertTrue(valueHit || anyStaticText,
                      "no panel element was assertable by ANY strategy")
    }

    /// Probe E — the surfaces issue #766 actually needs: with a POPULATED roster, are the
    /// interactive per-row elements (the switch chip / roster rows) reachable?
    ///
    /// Round 2 measured a panel in its `.notRunning` state, which renders header + a Start card and
    /// NO roster at all — so `buttons=0` there was the product's own emptiness, not evidence about
    /// reachability. This probe only means anything when a stub daemon is serving.
    func testPopulatedRosterInteractionSurfaces() throws {
        let app = launch("E.roster")
        defer { app.terminate() }
        guard openPanel(app, "E.roster") else { return XCTFail("panel never opened") }

        // Give the watch stream a moment to deliver the snapshot and SwiftUI a moment to lay out.
        _ = app.staticTexts.firstMatch.waitForExistence(timeout: 10)
        Thread.sleep(forTimeInterval: 3)

        let texts = app.staticTexts
        var values: [String] = []
        for i in 0..<min(texts.count, 25) {
            let e = texts.element(boundBy: i)
            values.append("[\(e.value as? String ?? e.label)]")
        }
        note("E.roster staticTexts=\(texts.count) buttons=\(app.buttons.count) " +
             "checkBoxes=\(app.checkBoxes.count) images=\(app.images.count) " +
             "cells=\(app.cells.count) groups=\(app.groups.count)")
        note("E.roster values=\(values.joined(separator: " "))")
        note("E.roster FULL TREE >>>\n\(app.debugDescription)\n<<< END TREE")

        // Diagnostic only — this probe REPORTS the interaction surface, it does not gate on a
        // guess about what the stub renders. The tree dump above is the evidence.
        XCTAssertTrue(app.dialogs.firstMatch.exists, "panel dialog vanished before inspection")
    }
}
