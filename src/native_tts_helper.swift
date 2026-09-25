import AVFoundation
import Foundation

// This helper is embedded in the Rust executable. It writes raw 24 kHz mono
// PCM16 to stdout as AVSpeechSynthesizer produces buffers. Diagnostics use stderr.
if CommandLine.arguments.contains("--voices") {
    let voices = AVSpeechSynthesisVoice.speechVoices().map { voice in
        ["id": voice.identifier, "name": voice.name, "language": voice.language]
    }
    let data = try JSONSerialization.data(withJSONObject: voices)
    FileHandle.standardOutput.write(data)
    exit(0)
}

guard CommandLine.arguments.count == 7 else {
    fputs("usage: native-tts voice rate pitch volume pre-delay post-delay\n", stderr)
    exit(2)
}

let args = CommandLine.arguments
guard let rate = Float(args[2]), (0...1).contains(rate),
      let pitch = Float(args[3]), (0.5...2).contains(pitch),
      let volume = Float(args[4]), (0...1).contains(volume),
      let preDelay = Double(args[5]), preDelay >= 0, preDelay.isFinite,
      let postDelay = Double(args[6]), postDelay >= 0, postDelay.isFinite else {
    fputs("invalid speech parameters\n", stderr)
    exit(2)
}

let text = String(data: FileHandle.standardInput.readDataToEndOfFile(), encoding: .utf8) ?? ""
guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
    fputs("speech text is empty\n", stderr)
    exit(2)
}

let utterance = AVSpeechUtterance(string: text)
if !args[1].isEmpty {
    guard let voice = AVSpeechSynthesisVoice(identifier: args[1]) else {
        fputs("selected voice is unavailable: \(args[1])\n", stderr)
        exit(2)
    }
    utterance.voice = voice
}
utterance.rate = rate
utterance.pitchMultiplier = pitch
utterance.volume = volume
utterance.preUtteranceDelay = preDelay
utterance.postUtteranceDelay = postDelay

final class AudioSink {
    private let lock = NSLock()
    private(set) var finished = false
    private(set) var error: String?
    private var converter: AVAudioConverter?
    private var sourceFormat: AVAudioFormat?
    private let targetFormat = AVAudioFormat(
        commonFormat: .pcmFormatInt16,
        sampleRate: 24_000,
        channels: 1,
        interleaved: true
    )!

    func receive(_ buffer: AVAudioBuffer) {
        lock.lock()
        defer { lock.unlock() }
        guard !finished else { return }
        guard let pcm = buffer as? AVAudioPCMBuffer else {
            error = "speech engine returned a non-PCM buffer"
            finished = true
            return
        }
        if pcm.frameLength == 0 {
            finished = true
            return
        }
        if sourceFormat == nil || !pcm.format.isEqual(sourceFormat!) {
            sourceFormat = pcm.format
            converter = AVAudioConverter(from: pcm.format, to: targetFormat)
        }
        guard let converter else {
            error = "cannot convert speech audio to 24 kHz mono PCM16"
            finished = true
            return
        }

        var supplied = false
        while true {
            guard let output = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: 4096) else {
                error = "cannot allocate audio buffer"
                finished = true
                return
            }
            var conversionError: NSError?
            let status = converter.convert(to: output, error: &conversionError) { _, inputStatus in
                if supplied {
                    inputStatus.pointee = .noDataNow
                    return nil
                }
                supplied = true
                inputStatus.pointee = .haveData
                return pcm
            }
            if let conversionError {
                error = conversionError.localizedDescription
                finished = true
                return
            }
            if output.frameLength > 0 {
                let audio = output.audioBufferList.pointee.mBuffers
                if let data = audio.mData {
                    FileHandle.standardOutput.write(Data(bytes: data, count: Int(audio.mDataByteSize)))
                }
            }
            if status != .haveData || output.frameLength == 0 { break }
        }
    }

    func snapshot() -> (Bool, String?) {
        lock.lock()
        defer { lock.unlock() }
        return (finished, error)
    }
}

let sink = AudioSink()
let synthesizer = AVSpeechSynthesizer()
synthesizer.write(utterance, toBufferCallback: sink.receive)
let deadline = Date(timeIntervalSinceNow: 75)
while !sink.snapshot().0 && Date() < deadline {
    _ = RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.05))
}
let result = sink.snapshot()
if let error = result.1 {
    fputs("\(error)\n", stderr)
    exit(1)
}
if !result.0 {
    fputs("speech synthesis timed out\n", stderr)
    exit(1)
}
