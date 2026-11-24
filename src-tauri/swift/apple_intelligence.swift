import Dispatch
import Foundation
import FoundationModels

@available(macOS 26.0, *)
@Generable
private struct CleanedTranscript: Sendable {
    let cleanedText: String
}

// MARK: - Swift implementation for Apple LLM integration
// This file is compiled via Cargo build script for Apple Silicon targets

private typealias ResponsePointer = UnsafeMutablePointer<AppleLLMResponse>

private func duplicateCString(_ text: String) -> UnsafeMutablePointer<CChar>? {
    return text.withCString { basePointer in
        guard let duplicated = strdup(basePointer) else {
            return nil
        }
        return duplicated
    }
}

private func truncatedText(_ text: String, limit: Int) -> String {
    guard limit > 0 else { return text }
    let words = text.split(
        maxSplits: .max,
        omittingEmptySubsequences: true,
        whereSeparator: { $0.isWhitespace || $0.isNewline }
    )
    if words.count <= limit {
        return text
    }
    return words.prefix(limit).joined(separator: " ")
}

@_cdecl("is_apple_intelligence_available")
public func isAppleIntelligenceAvailable() -> Int32 {
    guard #available(macOS 26.0, *) else {
        return 0
    }

    let model = SystemLanguageModel.default
    switch model.availability {
    case .available:
        return 1
    case .unavailable:
        return 0
    }
}

@_cdecl("process_text_with_system_prompt_apple")
public func processTextWithSystemPrompt(
    _ systemPrompt: UnsafePointer<CChar>,
    _ userContent: UnsafePointer<CChar>,
    maxTokens: Int32
) -> UnsafeMutablePointer<AppleLLMResponse> {
    return processTextWithOptions(
        systemPrompt,
        userContent,
        maxTokens: maxTokens,
        plainTextOnly: maxTokens < 0 ? 1 : 0,
        timeoutMs: 0
    )
}

@_cdecl("process_text_with_options_apple")
public func processTextWithOptions(
    _ systemPrompt: UnsafePointer<CChar>,
    _ userContent: UnsafePointer<CChar>,
    maxTokens: Int32,
    plainTextOnly: Int32,
    timeoutMs: Int32
) -> UnsafeMutablePointer<AppleLLMResponse> {
    let swiftSystemPrompt = String(cString: systemPrompt)
    let swiftUserContent = String(cString: userContent)
    let responsePtr = ResponsePointer.allocate(capacity: 1)
    responsePtr.initialize(to: AppleLLMResponse(response: nil, success: 0, error_message: nil))

    guard #available(macOS 26.0, *) else {
        responsePtr.pointee.error_message = duplicateCString(
            "Apple Intelligence requires macOS 26 or newer."
        )
        return responsePtr
    }

    let model = SystemLanguageModel.default
    guard model.availability == .available else {
        responsePtr.pointee.error_message = duplicateCString(
            "Apple Intelligence is not currently available on this device."
        )
        return responsePtr
    }

    let usePlainText = plainTextOnly != 0 || maxTokens < 0
    let tokenLimit = max(0, Int(maxTokens))
    let semaphore = DispatchSemaphore(value: 0)

    // Thread-safe container to pass results from async task back to calling thread.
    // Guarded by NSLock because a timed-out call cancels `genTask` and returns early
    // while FoundationModels finishes tearing down on its detached executor.
    final class ResultBox: @unchecked Sendable {
        private let lock = NSLock()
        private var responseValue: String?
        private var errorValue: String?

        func setResponse(_ value: String) {
            lock.lock()
            defer { lock.unlock() }
            responseValue = value
        }

        func setError(_ value: String) {
            lock.lock()
            defer { lock.unlock() }
            errorValue = value
        }

        func snapshot() -> (response: String?, error: String?) {
            lock.lock()
            defer { lock.unlock() }
            return (responseValue, errorValue)
        }
    }
    let box = ResultBox()

    let effectiveTimeoutMs = timeoutMs > 0 ? Int(timeoutMs) : (usePlainText ? 4200 : 2800)

    let genTask = Task.detached(priority: .userInitiated) {
        defer { semaphore.signal() }
        do {
            let session = LanguageModelSession(
                model: model,
                instructions: swiftSystemPrompt
            )
            let options = GenerationOptions(sampling: .greedy)
            var output: String

            if usePlainText {
                // Direct greedy plain-text generation skips @Generable JSON
                // string-escaping state-machine overhead (~2-3x faster on 3B).
                let generation = try await session.respond(
                    to: swiftUserContent,
                    options: options
                )
                output = generation.content
            } else {
                do {
                    let structured = try await session.respond(
                        to: swiftUserContent,
                        generating: CleanedTranscript.self,
                        options: options
                    )
                    output = structured.content.cleanedText
                } catch is CancellationError {
                    throw CancellationError()
                } catch {
                    try Task.checkCancellation()
                    guard swiftUserContent.count <= 600 else {
                        throw error
                    }
                    let fallbackSession = LanguageModelSession(
                        model: model,
                        instructions: swiftSystemPrompt
                    )
                    let fallbackGeneration = try await fallbackSession.respond(
                        to: swiftUserContent,
                        options: options
                    )
                    output = fallbackGeneration.content
                }
            }

            if tokenLimit > 0 {
                output = truncatedText(output, limit: tokenLimit)
            }
            box.setResponse(output)
        } catch {
            box.setError(error.localizedDescription)
        }
    }

    // Enforce interactive or batch SLA at the FFI boundary so stalls cancel cleanly.
    let waitResult = semaphore.wait(timeout: .now() + .milliseconds(effectiveTimeoutMs))
    if waitResult == .timedOut {
        genTask.cancel()
        responsePtr.pointee.error_message = duplicateCString(
            "Apple Intelligence exceeded \(Double(effectiveTimeoutMs) / 1000.0)s interactive budget; using instant local cleanup."
        )
        return responsePtr
    }

    let snapshot = box.snapshot()
    if let response = snapshot.response {
        responsePtr.pointee.response = duplicateCString(response)
        responsePtr.pointee.success = 1
    } else {
        responsePtr.pointee.error_message = duplicateCString(snapshot.error ?? "Unknown error")
    }

    return responsePtr
}

@_cdecl("free_apple_llm_response")
public func freeAppleLLMResponse(_ response: UnsafeMutablePointer<AppleLLMResponse>?) {
    guard let response = response else { return }

    if let responseStr = response.pointee.response {
        free(UnsafeMutablePointer(mutating: responseStr))
    }

    if let errorStr = response.pointee.error_message {
        free(UnsafeMutablePointer(mutating: errorStr))
    }

    response.deallocate()
}