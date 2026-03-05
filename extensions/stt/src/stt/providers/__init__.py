"""STT provider implementations."""

from .base import STTProvider
from .whisper import FasterWhisperProvider

__all__ = ["STTProvider", "FasterWhisperProvider"]
