package main

import (
	"fmt"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
	waLog "go.mau.fi/whatsmeow/util/log"
)

// waLogger bridges whatsmeow's internal log calls to mcplite structured logs.
type waLogger struct{ module string }

func (l waLogger) Debugf(msg string, args ...interface{}) {} // suppress debug noise
func (l waLogger) Infof(msg string, args ...interface{}) {
	mcplite.LogEvent("INFO", fmt.Sprintf(msg, args...), map[string]any{"service": "whatsapp", "module": l.module})
}
func (l waLogger) Warnf(msg string, args ...interface{}) {
	mcplite.LogEvent("WARN", fmt.Sprintf(msg, args...), map[string]any{"service": "whatsapp", "module": l.module})
}
func (l waLogger) Errorf(msg string, args ...interface{}) {
	mcplite.LogEvent("ERROR", fmt.Sprintf(msg, args...), map[string]any{"service": "whatsapp", "module": l.module})
}
func (l waLogger) Sub(module string) waLog.Logger {
	return waLogger{module: l.module + "/" + module}
}
