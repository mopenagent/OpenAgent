"""OpenTelemetry observability — dual export: OTLP/JSON files + optional Jaeger.

Each signal type gets its own daily-rotated JSONL file in the ``logs/``
directory (configurable via ``setup_otel``).

  logs/openagent-traces-YYYY-MM-DD.jsonl
  logs/openagent-logs-YYYY-MM-DD.jsonl
  logs/openagent-metrics-YYYY-MM-DD.jsonl

Files older than 1 day are deleted automatically on rotation.  Sampling is
100 % (AlwaysOnSampler).

Wire format: OTLP/JSON — identical to what an OTLP/HTTP collector expects.

Jaeger / collector export
--------------------------
Set ``OTEL_EXPORTER_OTLP_ENDPOINT`` to enable live export alongside the files::

    OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318  # Jaeger OTLP/HTTP

Requires ``opentelemetry-exporter-otlp-proto-http`` (already in requirements.txt).
The file export always runs; Jaeger is additive and best-effort.

Usage::

    from openagent.observability.otel import setup_otel, get_tracer, get_meter

    setup_otel(service_name="openagent", logs_dir=Path("logs"))

    tracer = get_tracer("openagent.loop")
    with tracer.start_as_current_span("agent.process") as span:
        span.set_attribute("session.key", session_key)
        span.set_attribute("platform", "whatsapp")

Trace IDs are also available for Baggage propagation::

    from openagent.observability.otel import baggage_set, baggage_get, current_trace_id
"""

from __future__ import annotations

import json
import logging
import os
import time
from datetime import date, datetime, timezone
from pathlib import Path
from threading import Lock
from typing import Any, Sequence

# ---------------------------------------------------------------------------
# Lazy OTEL imports — hard-fail with a clear message if sdk is missing
# ---------------------------------------------------------------------------

try:
    from opentelemetry import baggage as _baggage_api
    from opentelemetry import context as _otel_ctx
    from opentelemetry import trace
    from opentelemetry.sdk.resources import Resource
    from opentelemetry.sdk.trace import TracerProvider
    from opentelemetry.sdk.trace.export import (
        BatchSpanProcessor,
        SpanExporter,
        SpanExportResult,
    )
    from opentelemetry.sdk.trace.sampling import ALWAYS_ON
    from opentelemetry.sdk._logs import LoggerProvider, LoggingHandler
    from opentelemetry.sdk._logs.export import (
        BatchLogRecordProcessor,
        LogExporter,
        LogExportResult,
    )
    from opentelemetry.sdk.metrics import MeterProvider
    from opentelemetry.sdk.metrics.export import (
        MetricExporter,
        MetricExportResult,
        PeriodicExportingMetricReader,
    )
    _OTEL_AVAILABLE = True
except ImportError:  # pragma: no cover
    _OTEL_AVAILABLE = False

# Optional OTLP/HTTP exporters — present when opentelemetry-exporter-otlp-proto-http is installed
try:
    from opentelemetry.exporter.otlp.proto.http.trace_exporter import (
        OTLPSpanExporter as _OTLPSpanExporter,
    )
    try:
        from opentelemetry.exporter.otlp.proto.http.log_exporter import (
            OTLPLogExporter as _OTLPLogExporter,
        )
    except ImportError:
        from opentelemetry.exporter.otlp.proto.http._log_exporter import (  # type: ignore[no-redef]
            OTLPLogExporter as _OTLPLogExporter,
        )
    from opentelemetry.exporter.otlp.proto.http.metric_exporter import (
        OTLPMetricExporter as _OTLPMetricExporter,
    )
    _OTLP_HTTP_AVAILABLE = True
except ImportError:
    _OTLP_HTTP_AVAILABLE = False

_log = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Daily rotating file writer (thread-safe)
# ---------------------------------------------------------------------------

class DailyFileWriter:
    """Thread-safe JSONL writer that rotates files daily and keeps 1 day."""

    def __init__(self, logs_dir: Path, prefix: str, suffix: str = ".jsonl") -> None:
        self._dir = logs_dir
        self._prefix = prefix
        self._suffix = suffix
        self._file = None
        self._date: date | None = None
        self._lock = Lock()

    # ------------------------------------------------------------------

    def _filename(self, d: date) -> Path:
        return self._dir / f"{self._prefix}-{d.isoformat()}{self._suffix}"

    def _open_for_today(self) -> None:
        today = date.today()
        if self._date == today:
            return
        if self._file is not None:
            try:
                self._file.close()
            except OSError:
                pass
        self._dir.mkdir(parents=True, exist_ok=True)
        self._file = open(self._filename(today), "a", encoding="utf-8")  # noqa: WPS515
        self._date = today
        self._purge_old()

    def _purge_old(self) -> None:
        """Remove files older than 1 day."""
        assert self._date is not None
        for p in self._dir.glob(f"{self._prefix}-*{self._suffix}"):
            stem = p.stem  # e.g. "openagent-traces-2026-03-07"
            date_part = stem[len(self._prefix) + 1:]
            try:
                file_date = date.fromisoformat(date_part)
                if (self._date - file_date).days > 1:
                    p.unlink(missing_ok=True)
            except (ValueError, OSError):
                pass

    # ------------------------------------------------------------------

    def write_line(self, payload: str) -> None:
        with self._lock:
            self._open_for_today()
            if self._file is None:
                return
            self._file.write(payload)
            if not payload.endswith("\n"):
                self._file.write("\n")
            self._file.flush()

    def close(self) -> None:
        with self._lock:
            if self._file is not None:
                try:
                    self._file.close()
                except OSError:
                    pass
                self._file = None


# ---------------------------------------------------------------------------
# OTLP/JSON helpers
# ---------------------------------------------------------------------------

def _ns_str(ns: int | None) -> str:
    """Nanoseconds as string (JSON int64 compat)."""
    return str(ns or 0)


def _attr_value(v: Any) -> dict:
    if isinstance(v, bool):
        return {"boolValue": v}
    if isinstance(v, int):
        return {"intValue": str(v)}
    if isinstance(v, float):
        return {"doubleValue": v}
    if isinstance(v, str):
        return {"stringValue": v}
    if isinstance(v, (list, tuple)):
        return {"arrayValue": {"values": [_attr_value(i) for i in v]}}
    return {"stringValue": str(v)}


def _attrs(attrs: dict | None) -> list:
    if not attrs:
        return []
    return [{"key": k, "value": _attr_value(v)} for k, v in attrs.items()]


def _trace_id_hex(tid: int) -> str:
    return format(tid, "032x")


def _span_id_hex(sid: int) -> str:
    return format(sid, "016x")


def _span_kind_int(kind: Any) -> int:
    if not _OTEL_AVAILABLE:
        return 0
    from opentelemetry.trace import SpanKind
    return {
        SpanKind.INTERNAL: 1,
        SpanKind.SERVER: 2,
        SpanKind.CLIENT: 3,
        SpanKind.PRODUCER: 4,
        SpanKind.CONSUMER: 5,
    }.get(kind, 0)


def _status_code_int(status: Any) -> int:
    if not _OTEL_AVAILABLE:
        return 0
    from opentelemetry.trace import StatusCode
    return {
        StatusCode.OK: 1,
        StatusCode.ERROR: 2,
    }.get(status.status_code, 0)


def _resource_attrs(resource: Any) -> dict:
    return {"attributes": _attrs(dict(resource.attributes))}


def _serialize_span(span: Any) -> dict:
    ctx = span.context
    d: dict[str, Any] = {
        "traceId": _trace_id_hex(ctx.trace_id),
        "spanId": _span_id_hex(ctx.span_id),
        "name": span.name,
        "kind": _span_kind_int(span.kind),
        "startTimeUnixNano": _ns_str(span.start_time),
        "endTimeUnixNano": _ns_str(span.end_time),
        "attributes": _attrs(dict(span.attributes) if span.attributes else {}),
        "events": [_serialize_event(e) for e in (span.events or [])],
        "status": {"code": _status_code_int(span.status)},
    }
    if span.parent and span.parent.is_valid:
        d["parentSpanId"] = _span_id_hex(span.parent.span_id)
    if span.status.description:
        d["status"]["message"] = span.status.description
    return d


def _serialize_event(event: Any) -> dict:
    return {
        "timeUnixNano": _ns_str(event.timestamp),
        "name": event.name,
        "attributes": _attrs(dict(event.attributes) if event.attributes else {}),
    }


# ---------------------------------------------------------------------------
# Span exporter
# ---------------------------------------------------------------------------

class OTLPJsonSpanExporter(SpanExporter):
    """Writes spans to a daily-rotated OTLP/JSON file."""

    def __init__(self, writer: DailyFileWriter) -> None:
        self._writer = writer

    def export(self, spans: Sequence[Any]) -> Any:
        if not spans:
            return SpanExportResult.SUCCESS
        # Group by resource + instrumentation scope
        by_resource: dict[str, dict] = {}
        for span in spans:
            res_key = id(span.resource)
            if res_key not in by_resource:
                by_resource[res_key] = {
                    "resource": _resource_attrs(span.resource),
                    "scopeSpans": {},
                    "_res": span.resource,
                }
            scope = span.instrumentation_scope
            scope_key = (scope.name if scope else "", scope.version if scope else "")
            if scope_key not in by_resource[res_key]["scopeSpans"]:
                by_resource[res_key]["scopeSpans"][scope_key] = {
                    "scope": {
                        "name": scope_key[0],
                        "version": scope_key[1] or "",
                    },
                    "spans": [],
                }
            by_resource[res_key]["scopeSpans"][scope_key]["spans"].append(
                _serialize_span(span)
            )

        resource_spans = []
        for entry in by_resource.values():
            resource_spans.append({
                "resource": entry["resource"],
                "scopeSpans": list(entry["scopeSpans"].values()),
            })

        try:
            self._writer.write_line(json.dumps({"resourceSpans": resource_spans}))
        except Exception:  # noqa: BLE001
            _log.exception("OTEL span export failed")
            return SpanExportResult.FAILURE
        return SpanExportResult.SUCCESS

    def shutdown(self) -> None:
        self._writer.close()


# ---------------------------------------------------------------------------
# Log exporter
# ---------------------------------------------------------------------------

def _severity_number(level: int) -> int:
    """Map Python logging level to OTLP SeverityNumber."""
    if level >= 50:
        return 21  # FATAL
    if level >= 40:
        return 17  # ERROR
    if level >= 30:
        return 13  # WARN
    if level >= 20:
        return 9   # INFO
    if level >= 10:
        return 5   # DEBUG
    return 1       # TRACE


def _severity_text(level: int) -> str:
    return logging.getLevelName(level)


class OTLPJsonLogExporter(LogExporter):
    """Writes log records to a daily-rotated OTLP/JSON file."""

    def __init__(self, writer: DailyFileWriter) -> None:
        self._writer = writer

    def export(self, batch: Sequence[Any]) -> Any:
        if not batch:
            return LogExportResult.SUCCESS
        by_resource: dict[int, dict] = {}
        for readable in batch:
            res = getattr(readable, "resource", None)
            res_key = id(res)
            if res_key not in by_resource:
                by_resource[res_key] = {
                    "resource": _resource_attrs(res) if res else {"attributes": []},
                    "scopeLogs": {},
                }
            scope = getattr(readable, "instrumentation_scope", None)
            scope_name = scope.name if scope else ""
            if scope_name not in by_resource[res_key]["scopeLogs"]:
                by_resource[res_key]["scopeLogs"][scope_name] = {
                    "scope": {"name": scope_name},
                    "logRecords": [],
                }
            lr = self._serialize_record(readable)
            by_resource[res_key]["scopeLogs"][scope_name]["logRecords"].append(lr)

        resource_logs = [
            {
                "resource": e["resource"],
                "scopeLogs": list(e["scopeLogs"].values()),
            }
            for e in by_resource.values()
        ]
        try:
            self._writer.write_line(json.dumps({"resourceLogs": resource_logs}))
        except Exception:  # noqa: BLE001
            _log.exception("OTEL log export failed")
            return LogExportResult.FAILURE
        return LogExportResult.SUCCESS

    @staticmethod
    def _serialize_record(readable: Any) -> dict:
        # ReadableLogRecord wraps log_record; fall back to direct attrs for
        # alternative SDKs that expose fields directly.
        rec = getattr(readable, "log_record", readable)

        level = getattr(rec, "severity_number", None)
        if hasattr(level, "value"):
            sev_num = level.value
        elif isinstance(level, int):
            sev_num = level
        else:
            sev_num = 0

        body = getattr(rec, "body", None) or ""
        ts = getattr(rec, "timestamp", None) or getattr(rec, "time_unix_nano", None) or 0
        obs_ts = getattr(rec, "observed_timestamp", None) or ts

        lr: dict[str, Any] = {
            "timeUnixNano": _ns_str(ts),
            "observedTimeUnixNano": _ns_str(obs_ts),
            "severityNumber": sev_num,
            "severityText": getattr(rec, "severity_text", "") or "",
            "body": {"stringValue": str(body)},
            "attributes": _attrs(
                dict(rec.attributes) if getattr(rec, "attributes", None) else {}
            ),
        }
        # Trace context from span (if inside a traced span)
        ctx = getattr(rec, "context", None)
        if ctx is not None:
            tid = getattr(ctx, "trace_id", 0) or 0
            sid = getattr(ctx, "span_id", 0) or 0
        else:
            tid = getattr(rec, "trace_id", 0) or 0
            sid = getattr(rec, "span_id", 0) or 0
        if tid:
            lr["traceId"] = _trace_id_hex(tid)
        if sid:
            lr["spanId"] = _span_id_hex(sid)
        return lr

    def shutdown(self) -> None:
        self._writer.close()


# ---------------------------------------------------------------------------
# Metric exporter
# ---------------------------------------------------------------------------

def _metric_attr_value(v: Any) -> dict:
    return _attr_value(v)


class OTLPJsonMetricExporter(MetricExporter):
    """Writes metrics to a daily-rotated OTLP/JSON file."""

    def __init__(self, writer: DailyFileWriter) -> None:
        from opentelemetry.sdk.metrics.export import AggregationTemporality
        from opentelemetry.sdk.metrics import (
            Counter, Histogram, ObservableCounter,
            ObservableGauge, ObservableUpDownCounter, UpDownCounter,
        )
        preferred = {
            Counter: AggregationTemporality.CUMULATIVE,
            UpDownCounter: AggregationTemporality.CUMULATIVE,
            Histogram: AggregationTemporality.CUMULATIVE,
            ObservableCounter: AggregationTemporality.CUMULATIVE,
            ObservableUpDownCounter: AggregationTemporality.CUMULATIVE,
            ObservableGauge: AggregationTemporality.CUMULATIVE,
        }
        super().__init__(preferred_temporality=preferred)
        self._writer = writer

    def export(self, metrics_data: Any, timeout_millis: float = 10_000, **kwargs: Any) -> Any:
        if metrics_data is None:
            return MetricExportResult.SUCCESS
        resource_metrics = []
        for rm in getattr(metrics_data, "resource_metrics", []):
            scope_metrics_out = []
            for sm in getattr(rm, "scope_metrics", []):
                metrics_out = []
                for metric in getattr(sm, "metrics", []):
                    serialized = self._serialize_metric(metric)
                    if serialized:
                        metrics_out.append(serialized)
                if metrics_out:
                    scope = sm.scope
                    scope_metrics_out.append({
                        "scope": {
                            "name": scope.name if scope else "",
                            "version": (scope.version or "") if scope else "",
                        },
                        "metrics": metrics_out,
                    })
            if scope_metrics_out:
                resource_metrics.append({
                    "resource": _resource_attrs(rm.resource),
                    "scopeMetrics": scope_metrics_out,
                })

        if not resource_metrics:
            return MetricExportResult.SUCCESS
        try:
            self._writer.write_line(json.dumps({"resourceMetrics": resource_metrics}))
        except Exception:  # noqa: BLE001
            _log.exception("OTEL metric export failed")
            return MetricExportResult.FAILURE
        return MetricExportResult.SUCCESS

    @staticmethod
    def _serialize_metric(metric: Any) -> dict | None:
        from opentelemetry.sdk.metrics.export import AggregationTemporality
        data = getattr(metric, "data", None)
        if data is None:
            return None
        base: dict[str, Any] = {
            "name": metric.name,
            "description": metric.description or "",
            "unit": metric.unit or "1",
        }
        # Determine temporality as int: 1=DELTA, 2=CUMULATIVE
        agg_temp = getattr(data, "aggregation_temporality", AggregationTemporality.CUMULATIVE)
        agg_temp_int = int(agg_temp)

        data_type = type(data).__name__
        if data_type == "Sum":
            base["sum"] = {
                "dataPoints": [
                    OTLPJsonMetricExporter._sum_point(dp) for dp in data.data_points
                ],
                "aggregationTemporality": agg_temp_int,
                "isMonotonic": getattr(data, "is_monotonic", True),
            }
        elif data_type == "Gauge":
            base["gauge"] = {
                "dataPoints": [
                    OTLPJsonMetricExporter._gauge_point(dp) for dp in data.data_points
                ],
            }
        elif data_type == "Histogram":
            base["histogram"] = {
                "dataPoints": [
                    OTLPJsonMetricExporter._hist_point(dp) for dp in data.data_points
                ],
                "aggregationTemporality": agg_temp_int,
            }
        else:
            return None
        return base

    @staticmethod
    def _sum_point(dp: Any) -> dict:
        d: dict[str, Any] = {
            "startTimeUnixNano": _ns_str(dp.start_time_unix_nano),
            "timeUnixNano": _ns_str(dp.time_unix_nano),
            "attributes": _attrs(dict(dp.attributes) if dp.attributes else {}),
        }
        v = dp.value
        if isinstance(v, int):
            d["asInt"] = str(v)
        else:
            d["asDouble"] = v
        return d

    @staticmethod
    def _gauge_point(dp: Any) -> dict:
        return OTLPJsonMetricExporter._sum_point(dp)

    @staticmethod
    def _hist_point(dp: Any) -> dict:
        return {
            "startTimeUnixNano": _ns_str(dp.start_time_unix_nano),
            "timeUnixNano": _ns_str(dp.time_unix_nano),
            "attributes": _attrs(dict(dp.attributes) if dp.attributes else {}),
            "count": str(dp.count),
            "sum": dp.sum,
            "bucketCounts": [str(c) for c in dp.bucket_counts],
            "explicitBounds": list(dp.explicit_bounds),
        }

    def shutdown(self, timeout_millis: float = 30_000, **kwargs: Any) -> None:
        self._writer.close()

    def force_flush(self, timeout_millis: float = 10_000) -> bool:
        return True


# ---------------------------------------------------------------------------
# Global setup
# ---------------------------------------------------------------------------

_writers: list[DailyFileWriter] = []
_providers: list[Any] = []  # tracer_provider, logger_provider, meter_provider


def setup_otel(
    *,
    service_name: str = "openagent",
    service_version: str = "0.1.0",
    logs_dir: Path | None = None,
    metric_export_interval_ms: int = 60_000,
    extra_resource_attrs: dict[str, str] | None = None,
) -> None:
    """Configure global OTEL providers. Call once at application startup."""
    if not _OTEL_AVAILABLE:
        _log.warning(
            "opentelemetry-sdk not installed — OTEL disabled. "
            "Run: pip install opentelemetry-api opentelemetry-sdk"
        )
        return

    logs_dir = logs_dir or Path(os.getenv("OPENAGENT_LOGS_DIR", "logs"))

    resource_attrs: dict[str, Any] = {
        "service.name": service_name,
        "service.version": service_version,
        "telemetry.sdk.name": "opentelemetry",
        "telemetry.sdk.language": "python",
    }
    if extra_resource_attrs:
        resource_attrs.update(extra_resource_attrs)
    resource = Resource(attributes=resource_attrs)

    # Resolve OTLP endpoint — controls whether Jaeger/collector export is enabled
    otlp_endpoint = os.getenv("OTEL_EXPORTER_OTLP_ENDPOINT", "").rstrip("/")
    use_otlp = bool(otlp_endpoint) and _OTLP_HTTP_AVAILABLE
    if otlp_endpoint and not _OTLP_HTTP_AVAILABLE:
        _log.warning(
            "OTEL_EXPORTER_OTLP_ENDPOINT is set but opentelemetry-exporter-otlp-proto-http "
            "is not installed — Jaeger export disabled.  Run: pip install "
            "opentelemetry-exporter-otlp-proto-http"
        )

    # -- Traces --
    span_writer = DailyFileWriter(logs_dir, f"{service_name}-traces")
    _writers.append(span_writer)
    tracer_provider = TracerProvider(resource=resource, sampler=ALWAYS_ON)
    tracer_provider.add_span_processor(BatchSpanProcessor(OTLPJsonSpanExporter(span_writer)))
    if use_otlp:
        try:
            tracer_provider.add_span_processor(
                BatchSpanProcessor(_OTLPSpanExporter(endpoint=f"{otlp_endpoint}/v1/traces"))
            )
        except Exception as exc:
            _log.warning("OTLP span exporter init failed — Jaeger traces disabled: %s", exc)
    trace.set_tracer_provider(tracer_provider)
    _providers.append(tracer_provider)

    # -- Logs --
    log_writer = DailyFileWriter(logs_dir, f"{service_name}-logs")
    _writers.append(log_writer)
    logger_provider = LoggerProvider(resource=resource)
    logger_provider.add_log_record_processor(BatchLogRecordProcessor(OTLPJsonLogExporter(log_writer)))
    if use_otlp:
        try:
            logger_provider.add_log_record_processor(
                BatchLogRecordProcessor(_OTLPLogExporter(endpoint=f"{otlp_endpoint}/v1/logs"))
            )
        except Exception as exc:
            _log.warning("OTLP log exporter init failed — Jaeger logs disabled: %s", exc)
    # Bridge Python logging → OTEL logs
    otel_handler = LoggingHandler(level=logging.NOTSET, logger_provider=logger_provider)
    logging.getLogger().addHandler(otel_handler)
    _providers.append(logger_provider)

    # -- Metrics --
    metric_writer = DailyFileWriter(logs_dir, f"{service_name}-metrics")
    _writers.append(metric_writer)
    metric_readers: list[Any] = [
        PeriodicExportingMetricReader(
            OTLPJsonMetricExporter(metric_writer),
            export_interval_millis=metric_export_interval_ms,
        )
    ]
    if use_otlp:
        try:
            metric_readers.append(
                PeriodicExportingMetricReader(
                    _OTLPMetricExporter(endpoint=f"{otlp_endpoint}/v1/metrics"),
                    export_interval_millis=metric_export_interval_ms,
                )
            )
        except Exception as exc:
            _log.warning("OTLP metric exporter init failed — Jaeger metrics disabled: %s", exc)
    meter_provider = MeterProvider(resource=resource, metric_readers=metric_readers)
    from opentelemetry import metrics
    metrics.set_meter_provider(meter_provider)
    _providers.append(meter_provider)

    _log.info(
        "OTEL initialized — service=%s logs_dir=%s jaeger=%s",
        service_name,
        logs_dir,
        otlp_endpoint or "disabled",
    )


def shutdown_otel() -> None:
    """Flush all providers and close file writers. Call at application shutdown."""
    for provider in _providers:
        try:
            if hasattr(provider, "shutdown"):
                provider.shutdown()
        except Exception:  # noqa: BLE001
            pass
    _providers.clear()
    for w in _writers:
        try:
            w.close()
        except Exception:  # noqa: BLE001
            pass
    _writers.clear()


# ---------------------------------------------------------------------------
# Convenience accessors
# ---------------------------------------------------------------------------

def get_tracer(name: str, version: str = "") -> Any:
    """Return a tracer. Noop tracer if OTEL is not available."""
    if not _OTEL_AVAILABLE:
        return _NoopTracer()
    return trace.get_tracer(name, version)


def get_meter(name: str, version: str = "") -> Any:
    """Return a meter. Noop meter if OTEL is not available."""
    if not _OTEL_AVAILABLE:
        return None
    from opentelemetry import metrics
    return metrics.get_meter(name, version)


def current_trace_id() -> str | None:
    """Return the current trace ID as a 32-char hex string, or None."""
    if not _OTEL_AVAILABLE:
        return None
    ctx = trace.get_current_span().get_span_context()
    if ctx and ctx.is_valid:
        return _trace_id_hex(ctx.trace_id)
    return None


def current_span_id() -> str | None:
    """Return the current span ID as a 16-char hex string, or None."""
    if not _OTEL_AVAILABLE:
        return None
    ctx = trace.get_current_span().get_span_context()
    if ctx and ctx.is_valid:
        return _span_id_hex(ctx.span_id)
    return None


def baggage_set(key: str, value: str) -> Any:
    """Set a baggage entry in the current context. Returns new context token."""
    if not _OTEL_AVAILABLE:
        return None
    new_ctx = _baggage_api.set_baggage(key, value)
    return _otel_ctx.attach(new_ctx)


def baggage_get(key: str) -> str | None:
    """Get a baggage value from the current context."""
    if not _OTEL_AVAILABLE:
        return None
    return _baggage_api.get_baggage(key)


# ---------------------------------------------------------------------------
# Noop tracer fallback (when opentelemetry-sdk is not installed)
# ---------------------------------------------------------------------------

class _NoopSpan:
    def set_attribute(self, key: str, value: Any) -> None: ...
    def add_event(self, name: str, attributes: dict | None = None) -> None: ...
    def record_exception(self, exc: Exception, attributes: dict | None = None) -> None: ...
    def set_status(self, status: Any, description: str | None = None) -> None: ...
    def __enter__(self) -> "_NoopSpan": return self
    def __exit__(self, *args: Any) -> None: ...


class _NoopTracer:
    def start_as_current_span(
        self, name: str, *, kind: Any = None, attributes: dict | None = None
    ) -> "_NoopSpan":
        return _NoopSpan()

    def start_span(self, name: str, **kwargs: Any) -> "_NoopSpan":
        return _NoopSpan()
