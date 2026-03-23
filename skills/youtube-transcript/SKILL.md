---
name: youtube-transcript
description: Fetch a YouTube video transcript, summarize into clean knowledge, persist to diary, and deliver as 2-minute TTS audio snippets. Use when the user wants to transcribe, summarize, or listen to a YouTube video.
hint: Call skill.read(name="youtube-transcript") to get the full fetch -> summarize -> diary -> TTS workflow.
allowed-tools: sandbox.execute, tts.synthesize
enforce: false
---

# YouTube Transcript -> Summary -> Audio

Converts any YouTube video into clean, dense knowledge delivered as 2-minute audio snippets.

**Pipeline:** Fetch transcript -> AI summarize (remove junk) -> write to diary -> chunk -> TTS

---

## Step 1: Fetch the Transcript

Call `sandbox.execute` with the script below. Replace `VIDEO_URL` with the actual URL.

```python
import subprocess, sys, json, re
subprocess.run([sys.executable, "-m", "pip", "install", "youtube-transcript-api", "-q"], capture_output=True)

VIDEO_URL = "https://youtube.com/watch?v=REPLACE_ME"  # <-- replace this

def extract_video_id(u):
    m = re.search(r'(?:v=|youtu\.be/|embed/|shorts/)([A-Za-z0-9_-]{11})', u)
    return m.group(1) if m else (u if re.match(r'^[A-Za-z0-9_-]{11}$', u) else u)

from youtube_transcript_api import YouTubeTranscriptApi, NoTranscriptFound, TranscriptsDisabled

video_id = extract_video_id(VIDEO_URL)
try:
    tl = YouTubeTranscriptApi.list_transcripts(video_id)
    try:
        t = tl.find_transcript(["en", "en-US", "en-GB"])
    except NoTranscriptFound:
        try:
            t = tl.find_generated_transcript(["en"])
        except NoTranscriptFound:
            t = next(iter(tl)).translate("en")
    segs = t.fetch()
    lang = t.language_code
except TranscriptsDisabled:
    print(json.dumps({"error": "transcripts_disabled"})); raise SystemExit
except Exception as e:
    print(json.dumps({"error": str(e)})); raise SystemExit

text = " ".join(s["text"] for s in segs)
text = re.sub(r"\[.*?\]", "", text)
text = re.sub(r"\s+", " ", text).strip()
last = segs[-1] if segs else {}
dur = round((last.get("start", 0) + last.get("duration", 0)) / 60, 1)

print(json.dumps({
    "video_id": video_id,
    "url": f"https://youtube.com/watch?v={video_id}",
    "language": lang,
    "full_text": text,
    "word_count": len(text.split()),
    "estimated_duration_min": dur,
}))
```

**Parse the response:**
- `full_text` — raw transcript text, ready to summarize
- `word_count` — helps gauge expected summary size
- `estimated_duration_min` — video length
- `error` key means transcript unavailable (see Troubleshooting)

The canonical script is also at `skills/youtube-transcript/scripts/fetch_transcript.py`.

---

## Step 2: Summarize — Remove the Junk

Condense `full_text` into clean, dense knowledge using your own reasoning. No tool call needed.

**Target:** 20-30% of original word count.
- 5 min video (~750 words raw) -> ~180-220 word summary
- 15 min video (~2200 words raw) -> ~450-650 word summary
- 45 min video (~6500 words raw) -> ~1300-2000 word summary

**Remove all of this:**
- Filler: "you know", "kind of", "like I said", "so basically", "right?", "okay so"
- Openers/outros: "Hey guys welcome back", "smash that like button", "don't forget to subscribe", "see you in the next one"
- Repetition: any idea stated more than once — keep the clearest version only
- Meta-commentary: "in today's video", "before we dive in", "let me know in the comments"
- Sponsors and ads: skip entirely, do not summarize
- Transitions without content: "moving on", "now let's talk about", "as you can see"

**Keep all of this:**
- Every concrete fact, number, statistic, or named claim
- All advice, steps, instructions, or how-to sequences
- Examples that genuinely clarify a concept (one per concept max)
- Conclusions, key takeaways, and recommendations
- Counterarguments the speaker explicitly addresses

**Write the summary as flowing prose**, not bullet points. Bullets fragment ideas; prose preserves reasoning chains.

---

## Step 3: Write to Diary

Include the full clean summary verbatim in your response using this format so Cortex auto-captures it:

```
## YouTube Summary: <title or topic>
Source: <youtube url>
Duration: <X min> | Words in summary: <N>

<full clean summary — prose, dense, no filler>
```

This ensures the knowledge is persisted in the diary and becomes searchable via `memory.search` in future sessions.

---

## Step 4: Chunk for 2-Minute Audio

Split the clean summary into chunks of **~280 words** (2 minutes at 140 wpm average speaking pace).

**Algorithm:**
1. Split summary into sentences at `.`, `?`, `!` boundaries
2. Accumulate sentences until word count >= 280 OR sentences exhausted
3. Never split mid-sentence — complete the sentence even if it slightly exceeds 280 words
4. Label each chunk: prepend `"Part 1 of N. "` to the text before synthesizing

**Example for a 650-word summary:**
- Part 1: sentences 1-12 (~285 words)
- Part 2: sentences 13-24 (~280 words)
- Part 3: sentences 25-end (~85 words, shorter last chunk is fine)

---

## Step 5: Synthesize Each Chunk

Call `tts.synthesize` once per chunk. Call sequentially — do not parallelize.

```json
{
  "tool": "tts.synthesize",
  "params": {
    "text": "Part 1 of 3. [chunk text here]",
    "voice": "af_sarah",
    "speed": 1.0
  }
}
```

**Voices:**
- `af_sarah` — clear, neutral (default)
- `af_nicole` — warmer, conversational
- `af_sky` — bright, energetic

Each call returns `{"path": "data/artifacts/tts/<timestamp>.wav", "sample_rate": 24000}`.

---

## Step 6: Reply to User

Structure your reply as:

```
**[Video topic] — [X] audio clips**

[2-3 sentence teaser of the key ideas]

Audio:
- Part 1 (~2 min): data/artifacts/tts/xxx.wav
- Part 2 (~2 min): data/artifacts/tts/yyy.wav
- Part 3 (~1.5 min): data/artifacts/tts/zzz.wav

Want me to go deeper on any section, search related topics in memory, or try a different voice?
```

---

## Troubleshooting

| Error | Cause | Action |
|-------|-------|--------|
| `transcripts_disabled` | Creator disabled captions | Cannot fetch — report to user |
| `no_transcript` | No captions in any language | Cannot fetch — report to user |
| `missing_dependency` | pip install failed in sandbox | Try `sandbox.shell` with `pip install youtube-transcript-api` first |
| `fetch_failed` | Network or API issue | Retry once; if persists, report the message |
| Empty `full_text` after cleaning | Video is music-only (all `[Music]` markers) | Report: "This video has no spoken content" |

**Private/age-restricted videos:** The API cannot access these. The error will be `fetch_failed` with a message about cookies or login. Report to user.

**Very long videos (60+ min):** `word_count` may exceed 10,000. Still summarize to 20-30%. You may end up with 6-8 audio clips — that is expected and correct.

---

## Installation

```bash
pip install youtube-transcript-api
```

No API key required. The library fetches captions directly from YouTube's public caption endpoint.
