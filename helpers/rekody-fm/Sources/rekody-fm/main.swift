import Foundation
import FoundationModels

// rekody-fm — on-device cleanup helper backed by Apple Foundation Models.
//
// Contract with the rekody Rust binary (AppleFoundationProvider):
//   rekody-fm --check
//       Probe availability. Prints "available" + exit 0 if the on-device model
//       is ready; prints "unavailable: <reason>" to stderr + exit 2 otherwise.
//
//   rekody-fm --system <prompt>
//       Read the transcript from stdin, run it through the model with the given
//       system prompt (permissive content-transformation guardrails), print the
//       cleaned text to stdout, exit 0. On guardrail refusal / model error,
//       exit 3 (stderr has detail). On unavailable, exit 2. Empty input → exit 0
//       with no output.
//
// Exit codes let rekody's provider chain fall through gracefully to the next
// provider (or raw transcript) on any non-zero result.

func warn(_ s: String) { FileHandle.standardError.write((s + "\n").data(using: .utf8)!) }

func reasonString(_ reason: SystemLanguageModel.Availability.UnavailableReason) -> String {
    switch reason {
    case .deviceNotEligible: return "device not eligible"
    case .appleIntelligenceNotEnabled: return "Apple Intelligence not enabled in System Settings"
    case .modelNotReady: return "model not ready (still downloading?)"
    @unknown default: return "unavailable"
    }
}

let args = CommandLine.arguments

// ── --check ──────────────────────────────────────────────────────────────────
if args.contains("--check") {
    switch SystemLanguageModel.default.availability {
    case .available:
        print("available")
        exit(0)
    case .unavailable(let reason):
        warn("unavailable: \(reasonString(reason))")
        exit(2)
    }
}

// ── cleanup run ────────────────────────────────────────────────────────────
var system = ""
if let i = args.firstIndex(of: "--system"), i + 1 < args.count {
    system = args[i + 1]
}

let transcript = String(data: FileHandle.standardInput.readDataToEndOfFile(), encoding: .utf8) ?? ""
let input = transcript.trimmingCharacters(in: .whitespacesAndNewlines)
if input.isEmpty {
    exit(0)
}

switch SystemLanguageModel.default.availability {
case .available:
    break
case .unavailable(let reason):
    warn("unavailable: \(reasonString(reason))")
    exit(2)
}

let session = LanguageModelSession(
    model: SystemLanguageModel(guardrails: .permissiveContentTransformations),
    instructions: system
)

do {
    let response = try await session.respond(to: input)
    // stdout is the cleaned text, verbatim. rekody trims/guards on its side.
    print(response.content)
    exit(0)
} catch {
    warn("error: \(error)")
    exit(3)
}
