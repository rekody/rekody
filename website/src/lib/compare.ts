// Comparison data: Rekody vs Apple Dictation, Wispr Flow, MacWhisper, Superwhisper.
//
// Policy (do not weaken it):
//   - Every competitor cell comes from that vendor's OFFICIAL site, docs, or
//     privacy policy. No reviews, no blog roundups, no guesses.
//   - If a vendor does not publish a fact, the cell is `na` and renders as a
//     dot. We never claim a competitor lacks something we could not verify.
//   - Rows where a competitor is better (languages, platforms) stay in.
//   - Research log with source URLs + access dates lives in the marketing
//     vault: rekody/marketing/launch-week/compare-research.md (Obsidian).
//
// Checked 2026-07-23. When re-verifying, update `checkedLabel` and the
// research log together.

export type CellKind = 'yes' | 'no' | 'text' | 'na';

export interface Cell {
  kind: CellKind;
  /** Full cell text (deep table on /compare). Optional for yes/no/na. */
  text?: string;
  /** Shorter override for the homepage compact table. Falls back to `text`. */
  short?: string;
  /** Footnote number rendered as a superscript on /compare only. */
  note?: number;
}

export interface Row {
  label: string;
  /** Included in the homepage compact table. */
  compact?: boolean;
  /** Cells in column order: Rekody, Apple Dictation, Wispr Flow, MacWhisper, Superwhisper. */
  cells: [Cell, Cell, Cell, Cell, Cell];
}

export const columns = ['Rekody', 'Apple Dictation', 'Wispr Flow', 'MacWhisper', 'Superwhisper'];

export const checkedLabel = 'Checked July 2026 against each vendor’s official pages.';
export const correctionsEmail = 'hi@rekody.com';

export const rows: Row[] = [
  {
    label: 'Price',
    compact: true,
    cells: [
      { kind: 'text', text: 'Free. The Mac app is free; the CLI is MIT open source.', short: 'Free' },
      { kind: 'text', text: 'Included with macOS.', short: 'Included with macOS' },
      { kind: 'text', text: '$15/mo, or $12/mo billed annually. Free tier: 2,000 words/week. 14-day Pro trial.', short: 'From $12/mo' },
      { kind: 'text', text: 'Free version. Pro is €64 once, with lifetime updates.', short: '€64 once, free version' },
      { kind: 'text', text: '$8.49/mo, $84.99/yr, or $249.99 once. Free tier that doesn’t expire.', short: 'From $8.49/mo, free tier' },
    ],
  },
  {
    label: 'Account required',
    cells: [
      { kind: 'text', text: 'No' },
      { kind: 'text', text: 'No' },
      { kind: 'text', text: 'Yes, sign-in required' },
      { kind: 'na', text: 'Pro unlocks with a license key', note: 5 },
      { kind: 'na', text: 'Pro unlocks with a license key', note: 5 },
    ],
  },
  {
    label: 'Works offline',
    compact: true,
    cells: [
      { kind: 'yes', text: 'Yes. Models download once at setup.', short: 'Yes' },
      { kind: 'text', text: 'Mostly. On Apple silicon, general dictation runs on-device in many languages with no internet. Intel Macs and other languages use Apple servers.', short: 'Mostly, on Apple silicon', note: 1 },
      { kind: 'no', text: 'No. Internet connection required to transcribe.', short: 'Needs internet' },
      { kind: 'yes', text: 'Yes, with local models.', short: 'Yes' },
      { kind: 'yes', text: 'Yes, with local models. Their docs note offline models run best on Apple Silicon.', short: 'Yes' },
    ],
  },
  {
    label: 'Where your speech is processed',
    compact: true,
    cells: [
      { kind: 'text', text: 'On your Mac by default. Optional cloud engines exist; they are labeled and only run if you pick one.', short: 'On your Mac by default', note: 3 },
      { kind: 'text', text: 'On-device on Apple silicon for supported languages, otherwise Apple servers. Keyboard settings shows which applies to you.', short: 'On-device or Apple servers', note: 1 },
      { kind: 'text', text: 'In the cloud. With cloud sync off, audio is processed in real time and discarded after each request.', short: 'Cloud' },
      { kind: 'text', text: 'On your Mac. Optional cloud services via your own API keys.', short: 'On your Mac, cloud optional' },
      { kind: 'text', text: 'Your choice per mode: local models on-device, or cloud models.', short: 'On-device or cloud, your pick' },
    ],
  },
  {
    label: 'Do they keep your audio',
    compact: true,
    cells: [
      { kind: 'text', text: 'Never. No audio saved, no analytics, no Rekody server involved.', short: 'Never. No telemetry either.' },
      { kind: 'text', text: 'Not unless you opt in to Improve Siri and Dictation. Transcripts of server requests can be kept up to two years under a rotating random identifier.', short: 'Not unless you opt in', note: 2 },
      { kind: 'text', text: 'Configurable. Zero-retention combo is Privacy Mode on plus cloud sync off; with cloud sync on, dictation data is stored on their servers.', short: 'Zero-retention mode available', note: 4 },
      { kind: 'text', text: 'No. Processing is local; their words: “without data ever leaving your Mac.”', short: 'No, stays on your Mac' },
      { kind: 'text', text: 'No. Their policy: no audio recordings collected, nothing stored on their servers, no usage data.', short: 'No, nothing on their servers' },
    ],
  },
  {
    label: 'Speech engine, as published',
    cells: [
      { kind: 'text', text: 'Local Whisper plus a local streaming engine. Models download once.' },
      { kind: 'text', text: 'Apple’s built-in dictation. On-device languages use downloadable speech models; the model itself is unnamed.' },
      { kind: 'text', text: 'Cloud AI. Models not named publicly.' },
      { kind: 'text', text: 'Local Whisper and Parakeet models. Optional cloud speech services (Deepgram, ElevenLabs, Gladia).' },
      { kind: 'text', text: 'Local Whisper-family and Parakeet models. Optional cloud models (Ultra, Nova, Scribe, S1-Voice).' },
    ],
  },
  {
    label: 'Custom dictionary',
    cells: [
      { kind: 'yes', text: 'Yes. Personal dictionary, built in.' },
      { kind: 'na' },
      { kind: 'yes', text: 'Yes. Auto-learns your words; personal dictionary plus snippets.' },
      { kind: 'na' },
      { kind: 'yes', text: 'Yes, as a Pro feature.' },
    ],
  },
  {
    label: 'Works in any app',
    compact: true,
    cells: [
      { kind: 'yes', text: 'Yes. Text lands at your cursor.', short: 'Yes, at your cursor' },
      { kind: 'yes', text: 'Yes. Anywhere you can type.', short: 'Yes' },
      { kind: 'yes', text: 'Yes. Every application on your computer or phone.', short: 'Yes' },
      { kind: 'yes', text: 'Yes. System-wide dictation.', short: 'Yes' },
      { kind: 'yes', text: 'Yes. Any site or app.', short: 'Yes' },
    ],
  },
  {
    label: 'Activation',
    cells: [
      { kind: 'text', text: 'Hold ⌥ Space and talk; release to insert. Quick-tap to latch hands-free, tap again to stop.' },
      { kind: 'text', text: 'Mic key, a customizable shortcut, or Edit > Start Dictation.' },
      { kind: 'text', text: 'Hold fn to talk; double-press to lock hands-free. Sessions cap at 20 minutes.' },
      { kind: 'text', text: 'A keyboard shortcut you configure in settings.' },
      { kind: 'text', text: 'Press ⌥ Space to start; push-to-talk hold also available.' },
    ],
  },
  {
    label: 'Words appear as you speak',
    compact: true,
    cells: [
      { kind: 'yes', text: 'Yes. Streaming preview in the pill while you talk.', short: 'Yes, streaming' },
      { kind: 'na', note: 8 },
      { kind: 'no', text: 'No, by design. Their docs: no live transcription; text is inserted after processing.', short: 'No, text lands after' },
      { kind: 'na', note: 6 },
      { kind: 'na' },
    ],
  },
  {
    label: 'Platforms',
    cells: [
      { kind: 'text', text: 'macOS 13+, Apple Silicon and Intel. Real-time streaming is Apple Silicon only; Intel Macs transcribe with Whisper.' },
      { kind: 'text', text: 'Built into macOS.' },
      { kind: 'text', text: 'macOS, Windows, iPhone, Android.' },
      { kind: 'text', text: 'macOS, plus a separate iOS app.' },
      { kind: 'text', text: 'macOS, Windows, iOS.' },
    ],
  },
  {
    label: 'Languages',
    compact: true,
    cells: [
      { kind: 'text', text: 'English today. If you dictate in another language, the others serve you better right now.', short: 'English today' },
      { kind: 'text', text: 'About 50 on Apple’s published list; about 35 of them on-device.', short: 'About 50', note: 7 },
      { kind: 'text', text: '100+ with automatic detection.', short: '100+' },
      { kind: 'text', text: '100.', short: '100' },
      { kind: 'text', text: '100+ languages and dialects, with translation to English.', short: '100+' },
    ],
  },
];

export const compactRows = rows.filter((r) => r.compact);

export const footnotes: { n: number; text: string }[] = [
  { n: 1, text: 'Apple: “On a Mac with Apple silicon, general text dictation requests… are processed on the device in many languages and no internet connection is required.” Dictating on an Intel-based Mac, in a language without on-device support, or into a search box sends utterances to Apple or the search provider.' },
  { n: 2, text: 'Apple: “Unless you opt in to Improve Siri and Dictation, your audio data is not stored by Apple.” Transcripts of server-processed requests “may be retained… for up to two years” under “a random, device-generated identifier” not tied to your Apple Account.' },
  { n: 3, text: 'Rekody’s default engine is local (stt_engine = "local"). The CLI config also offers an optional cloud engine (Groq Whisper); it never runs unless you pick it, and there is no Rekody server in either path.' },
  { n: 4, text: 'Wispr Flow documents two independent controls: Privacy Mode (whether your data trains models) and Private Cloud Sync (whether dictation data is stored on their servers). Their docs call Privacy Mode on plus Cloud Sync off “Zero Data Retention.” Defaults are not published.' },
  { n: 5, text: 'Neither MacWhisper nor Superwhisper publishes whether the free tier needs a sign-in, so we leave that unclaimed. Both sell Pro as a license key.' },
  { n: 6, text: 'MacWhisper’s homepage calls the feature “Real-time dictation” but does not say whether words render while you are still speaking, so we leave the cell unclaimed.' },
  { n: 7, text: 'Counted from the Dictation sections of Apple’s macOS Feature Availability page, July 2026. The on-device subset is listed as “Dictation: On-Device and Modeless Dictation.”' },
  { n: 8, text: 'Apple’s current Mac User Guide describes dictation feedback (the pulsing cursor) but does not state whether words render while you speak, so we leave the cell unclaimed.' },
];

export const sources: { vendor: string; links: { label: string; href: string }[] }[] = [
  {
    vendor: 'Apple',
    links: [
      { label: 'Dictate messages and documents on Mac (User Guide)', href: 'https://support.apple.com/guide/mac-help/use-dictation-mh40584/mac' },
      { label: 'Ask Siri, Dictation & Privacy', href: 'https://www.apple.com/legal/privacy/data/en/ask-siri-dictation/' },
      { label: 'macOS Feature Availability (Dictation languages)', href: 'https://www.apple.com/macos/feature-availability/' },
    ],
  },
  {
    vendor: 'Wispr Flow',
    links: [
      { label: 'wisprflow.ai', href: 'https://wisprflow.ai/' },
      { label: 'Pricing', href: 'https://wisprflow.ai/pricing' },
      { label: 'Privacy Policy', href: 'https://wisprflow.ai/privacy-policy' },
      { label: 'Help: internet connection required', href: 'https://docs.wisprflow.ai/articles/5094956927-fix-no-internet-connection-issues' },
      { label: 'Help: Privacy Mode and Cloud Sync', href: 'https://docs.wisprflow.ai/articles/4709791908-understanding-privacy-mode-and-cloud-sync' },
      { label: 'Help: hands-free and push-to-talk', href: 'https://docs.wisprflow.ai/articles/6391241694-use-flow-hands-free' },
      { label: 'Help: why Flow doesn’t show words while you’re speaking', href: 'https://docs.wisprflow.ai/articles/7419492456-why-flow-doesn-t-show-words-while-you-re-speaking' },
    ],
  },
  {
    vendor: 'MacWhisper',
    links: [
      { label: 'macwhisper.com', href: 'https://www.macwhisper.com/' },
      { label: 'Docs: how to use the Dictation feature', href: 'https://docs.macwhisper.com/article/14-how-to-use-the-dictation-feature' },
      { label: 'Docs: MacWhisper for iOS', href: 'https://docs.macwhisper.com/article/33-macwhisper-for-ios' },
    ],
  },
  {
    vendor: 'Superwhisper',
    links: [
      { label: 'superwhisper.com', href: 'https://superwhisper.com/' },
      { label: 'AI models in Superwhisper', href: 'https://superwhisper.com/models' },
      { label: 'Privacy Policy', href: 'https://superwhisper.com/privacy' },
      { label: 'Docs: Superwhisper Pro', href: 'https://superwhisper.com/docs/get-started/sw-pro' },
      { label: 'Superwhisper for Windows', href: 'https://superwhisper.com/windows' },
    ],
  },
  {
    vendor: 'Rekody',
    links: [
      { label: 'Privacy', href: '/privacy' },
      { label: 'Open-source repo (engine defaults in config/default.toml)', href: 'https://github.com/rekody/rekody' },
    ],
  },
];
