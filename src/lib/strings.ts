// User-facing copy in one map (DESIGN 8), so the Spanish pass later is cheap.
// ponytail: screens keep short inline labels; titles, empty states and helper lines live here.
export const S = {
  nav: {
    home: "Home",
    history: "History",
    dictionary: "Dictionary",
    styles: "Styles",
    providers: "Providers",
    audio: "Audio",
    shortcuts: "Shortcuts",
    settings: "Settings",
  },
  title: {
    history: ["", "Every", " word you said."],
    dictionary: ["Your ", "vocabulary."],
    styles: ["How you ", "sound."],
    providers: ["Where your ", "voice", " goes."],
    audio: ["Your ", "microphone", ", handled."],
    shortcuts: ["Keys, ", "your", " way."],
    settings: ["The ", "quiet", " details."],
  } as Record<string, string[]>,
  empty: {
    recent: { line: ["Nothing said ", "yet."], help: (key: string) => `Hold ${key} anywhere and talk.` },
    history: { line: ["", "Quiet", " in here."], help: "Your dictations show up here." },
    search: { help: "Search covers raw and formatted text." },
    words: { line: ["Teach it your ", "words."], help: "Names, brands, jargon. susurro hints them to every provider." },
    corrections: { line: ["Fix what it ", "mishears."], help: "“contra flow” → “Contaflow”, every time." },
    snippets: { line: ["Say less, ", "paste more."], help: "A short phrase expands into any text." },
    devices: { line: ["No microphone ", "found."], help: "Plug one in. susurro picks it up right away." },
  },
  help: {
    noKeys: "needs key",
    cleanupFails: "If cleanup fails, susurro pastes the raw transcript.",
    localServer: "whisper-server :8080 or speaches :8000. LM Studio has no transcription endpoint.",
    openrouterStt: "OpenRouter ignores the vocabulary prompt. Your words still apply in cleanup and corrections.",
    language: "One language is more accurate than auto.",
    toneNeedsLlm: "Tone rewrites need an LLM. Punctuation and casing rules still apply.",
    bluetooth: "Bluetooth lowers mic quality while in use.",
    typing: "assumed typing baseline: 45 wpm",
  },
};
