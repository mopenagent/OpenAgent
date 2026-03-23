#!/usr/bin/env python3
"""
YouTube transcript fetcher.

Run via sandbox.execute (paste this code with VIDEO_URL replaced), or directly:
    python3 fetch_transcript.py <youtube-url-or-video-id>

Output (JSON to stdout):
    {
      "video_id": "dQw4w9WgXcQ",
      "url": "https://youtube.com/watch?v=dQw4w9WgXcQ",
      "language": "en",
      "full_text": "Complete raw transcript as a single string ...",
      "word_count": 1823,
      "estimated_duration_min": 14.2,
      "segment_count": 312
    }

Error output (JSON, exit 0 so sandbox captures it):
    {"error": "transcripts_disabled", "message": "..."}
    {"error": "no_transcript",        "message": "..."}
    {"error": "missing_dependency",   "message": "pip install youtube-transcript-api"}
    {"error": "fetch_failed",         "message": "..."}
"""

import subprocess
import sys
import json
import re

# ── Auto-install inside sandbox VM ──────────────────────────────────────────
subprocess.run(
    [sys.executable, "-m", "pip", "install", "youtube-transcript-api", "-q"],
    capture_output=True,
)

# ── Video ID extraction ──────────────────────────────────────────────────────

def extract_video_id(url_or_id: str) -> str:
    """Return the 11-char video ID from any YouTube URL form, or the input as-is."""
    match = re.search(r'(?:v=|youtu\.be/|embed/|shorts/)([A-Za-z0-9_-]{11})', url_or_id)
    if match:
        return match.group(1)
    if re.match(r'^[A-Za-z0-9_-]{11}$', url_or_id):
        return url_or_id
    return url_or_id


# ── Accept URL from CLI arg or inline replacement ────────────────────────────
VIDEO_URL = sys.argv[1] if len(sys.argv) > 1 else "<REPLACE_WITH_YOUTUBE_URL>"

if VIDEO_URL.startswith("<"):
    print(json.dumps({"error": "usage", "message": "Provide a YouTube URL as argument or replace VIDEO_URL inline"}))
    sys.exit(0)

video_id = extract_video_id(VIDEO_URL)

# ── Fetch transcript ─────────────────────────────────────────────────────────
try:
    from youtube_transcript_api import (
        YouTubeTranscriptApi,
        NoTranscriptFound,
        TranscriptsDisabled,
    )
except ImportError:
    print(json.dumps({
        "error": "missing_dependency",
        "message": "pip install youtube-transcript-api",
    }))
    sys.exit(0)

try:
    transcript_list = YouTubeTranscriptApi.list_transcripts(video_id)

    # Prefer manual English, then auto-generated English, then translate any available
    try:
        transcript = transcript_list.find_transcript(["en", "en-US", "en-GB"])
    except NoTranscriptFound:
        try:
            transcript = transcript_list.find_generated_transcript(["en", "en-US"])
        except NoTranscriptFound:
            # Take the first available and translate to English
            transcript = next(iter(transcript_list))
            transcript = transcript.translate("en")

    segments = transcript.fetch()
    language = transcript.language_code

except TranscriptsDisabled:
    print(json.dumps({
        "error": "transcripts_disabled",
        "message": f"Transcripts are disabled for video '{video_id}'. Try another video.",
    }))
    sys.exit(0)
except NoTranscriptFound:
    print(json.dumps({
        "error": "no_transcript",
        "message": f"No transcript found for video '{video_id}' in any language.",
    }))
    sys.exit(0)
except Exception as e:
    print(json.dumps({"error": "fetch_failed", "message": str(e)}))
    sys.exit(0)

# ── Clean raw text ───────────────────────────────────────────────────────────
full_text = " ".join(seg["text"].strip() for seg in segments)
# Remove sound/music markers: [Music], [Applause], [Laughter], etc.
full_text = re.sub(r"\[.*?\]", "", full_text)
# Collapse whitespace
full_text = re.sub(r"\s+", " ", full_text).strip()

word_count = len(full_text.split())
last_seg = segments[-1] if segments else {}
duration_secs = last_seg.get("start", 0) + last_seg.get("duration", 0)

print(json.dumps({
    "video_id": video_id,
    "url": f"https://youtube.com/watch?v={video_id}",
    "language": language,
    "full_text": full_text,
    "word_count": word_count,
    "estimated_duration_min": round(duration_secs / 60, 1),
    "segment_count": len(segments),
}, ensure_ascii=False))
