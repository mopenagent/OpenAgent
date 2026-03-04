package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/kmaneesh/openagent/services/sdk-go/mcplite"
)

const (
	defaultSocketPath = "data/sockets/filesystem.sock"
	defaultMaxBytes   = 64 * 1024
	maxBytesHardCap   = 1 * 1024 * 1024
)

var (
	defaultSkipDirs = map[string]struct{}{
		".git":         {},
		"node_modules": {},
		".venv":        {},
		"venv":         {},
		"dist":         {},
		"build":        {},
		"bin":          {},
		"__pycache__":  {},
	}
	errResultLimitReached = errors.New("result limit reached")
)

type filesystemRuntime struct {
	root string
}

type searchOptions struct {
	Root          string
	Query         string
	MaxResults    int
	IncludeHidden bool
	CaseSensitive bool
	Extensions    map[string]struct{}
}

type searchHit struct {
	Path string `json:"path"`
	Type string `json:"type"`
}

type searchResult struct {
	Root      string      `json:"root"`
	Query     string      `json:"query"`
	Hits      []searchHit `json:"hits"`
	Truncated bool        `json:"truncated"`
}

type fileStat struct {
	Path        string `json:"path"`
	Type        string `json:"type"`
	Size        int64  `json:"size"`
	Mode        string `json:"mode"`
	ModifiedUTC string `json:"modified_utc"`
}

func main() {
	if err := run(); err != nil {
		log.Fatalf("filesystem service failed: %v", err)
	}
}

func run() error {
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	socketPath := os.Getenv("OPENAGENT_SOCKET_PATH")
	if socketPath == "" {
		socketPath = defaultSocketPath
	}

	runtime, err := newFilesystemRuntime()
	if err != nil {
		return err
	}

	if err := os.MkdirAll(filepath.Dir(socketPath), 0o755); err != nil {
		return fmt.Errorf("create socket directory: %w", err)
	}
	if err := os.Remove(socketPath); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove stale socket: %w", err)
	}

	listener, err := net.Listen("unix", socketPath)
	if err != nil {
		return fmt.Errorf("listen on socket %q: %w", socketPath, err)
	}
	defer func() {
		_ = listener.Close()
		_ = os.Remove(socketPath)
	}()
	mcplite.LogEvent("INFO", "service listening", map[string]any{
		"service":     "filesystem",
		"socket_path": socketPath,
		"root":        runtime.root,
	})

	server := buildServer(runtime)
	var connWG sync.WaitGroup

	go func() {
		<-ctx.Done()
		_ = listener.Close()
	}()

	for {
		conn, acceptErr := listener.Accept()
		if acceptErr != nil {
			if errors.Is(acceptErr, net.ErrClosed) || ctx.Err() != nil {
				break
			}
			mcplite.LogEvent("ERROR", "accept failed", map[string]any{
				"service": "filesystem",
				"error":   acceptErr.Error(),
			})
			continue
		}

		connWG.Add(1)
		go func(c net.Conn) {
			defer connWG.Done()
			handleConn(ctx, c, server)
		}(conn)
	}

	connWG.Wait()
	return nil
}

func newFilesystemRuntime() (*filesystemRuntime, error) {
	root := strings.TrimSpace(os.Getenv("OPENAGENT_FILESYSTEM_ROOT"))
	if root == "" {
		cwd, err := os.Getwd()
		if err != nil {
			return nil, fmt.Errorf("resolve cwd: %w", err)
		}
		root = cwd
	}
	abs, err := filepath.Abs(root)
	if err != nil {
		return nil, fmt.Errorf("resolve root: %w", err)
	}
	st, err := os.Stat(abs)
	if err != nil {
		return nil, fmt.Errorf("stat root: %w", err)
	}
	if !st.IsDir() {
		return nil, errors.New("OPENAGENT_FILESYSTEM_ROOT must be a directory")
	}
	return &filesystemRuntime{root: filepath.Clean(abs)}, nil
}

func buildServer(rt *filesystemRuntime) *mcplite.Server {
	tools := []mcplite.ToolDefinition{
		toolDef("filesystem.search", "Search local filesystem paths by substring match with deterministic limits.", map[string]any{
			"query":          stringProp("Substring to match against relative file paths."),
			"root":           stringProp("Root directory to search from. Defaults to service root."),
			"max_results":    intProp("Maximum matches (1-500, default 50)."),
			"include_hidden": boolProp("Include hidden paths. Default false."),
			"case_sensitive": boolProp("Case-sensitive match. Default false."),
			"extensions": map[string]any{
				"type":        "array",
				"items":       map[string]any{"type": "string"},
				"description": "Optional extension allow-list like ['.go','.py'].",
			},
		}, []string{"query"}),
		toolDef("filesystem.list_dir", "List directory entries.", map[string]any{
			"path":           stringProp("Directory path relative to service root."),
			"include_hidden": boolProp("Include hidden entries. Default false."),
			"max_entries":    intProp("Maximum entries to return (1-1000, default 200)."),
		}, []string{"path"}),
		toolDef("filesystem.read_file", "Read text file content with optional slice.", map[string]any{
			"path":      stringProp("File path relative to service root."),
			"offset":    intProp("Start byte offset, default 0."),
			"max_bytes": intProp("Maximum bytes to read, default 65536, max 1048576."),
		}, []string{"path"}),
		toolDef("filesystem.write_file", "Write content to a file, creating parent directories by default.", map[string]any{
			"path":      stringProp("File path relative to service root."),
			"content":   stringProp("Content to write."),
			"mkdir":     boolProp("Create parent directories. Default true."),
			"overwrite": boolProp("Overwrite existing file. Default true."),
		}, []string{"path", "content"}),
		toolDef("filesystem.append_file", "Append content to a file.", map[string]any{
			"path":    stringProp("File path relative to service root."),
			"content": stringProp("Content to append."),
			"mkdir":   boolProp("Create parent directories. Default true."),
		}, []string{"path", "content"}),
		toolDef("filesystem.edit_file", "Replace exact text in file. Fails if old_text missing or ambiguous unless replace_all=true.", map[string]any{
			"path":        stringProp("File path relative to service root."),
			"old_text":    stringProp("Exact text to replace."),
			"new_text":    stringProp("Replacement text."),
			"replace_all": boolProp("Replace all occurrences. Default false."),
		}, []string{"path", "old_text", "new_text"}),
		toolDef("filesystem.stat", "Return file or directory metadata.", map[string]any{
			"path": stringProp("Path relative to service root."),
		}, []string{"path"}),
		toolDef("filesystem.mkdir", "Create a directory.", map[string]any{
			"path":      stringProp("Directory path relative to service root."),
			"recursive": boolProp("Create parents recursively. Default true."),
		}, []string{"path"}),
		toolDef("filesystem.delete", "Delete file or directory.", map[string]any{
			"path":      stringProp("Path relative to service root."),
			"recursive": boolProp("Allow recursive directory delete. Default false."),
		}, []string{"path"}),
		toolDef("filesystem.move", "Move/rename file or directory.", map[string]any{
			"src": stringProp("Source path relative to service root."),
			"dst": stringProp("Destination path relative to service root."),
		}, []string{"src", "dst"}),
		toolDef("filesystem.copy", "Copy file or directory recursively.", map[string]any{
			"src": stringProp("Source path relative to service root."),
			"dst": stringProp("Destination path relative to service root."),
		}, []string{"src", "dst"}),
		toolDef("filesystem.glob", "List paths that match a glob pattern.", map[string]any{
			"pattern":        stringProp("Glob pattern like '**/*.go' is not supported; use standard filepath globs."),
			"root":           stringProp("Root path relative to service root. Default service root."),
			"include_hidden": boolProp("Include hidden entries in result filtering. Default false."),
			"max_results":    intProp("Maximum matches to return (1-1000, default 200)."),
		}, []string{"pattern"}),
	}

	server := mcplite.NewServer(tools, "ready")
	server.RegisterToolHandler("filesystem.search", func(_ context.Context, params map[string]any) (string, error) {
		opts, err := parseSearchOptions(rt, params)
		if err != nil {
			return "", err
		}
		result, err := searchFilesystem(opts)
		if err != nil {
			return "", err
		}
		return marshalAny(result)
	})
	server.RegisterToolHandler("filesystem.list_dir", func(_ context.Context, params map[string]any) (string, error) {
		return rt.listDir(params)
	})
	server.RegisterToolHandler("filesystem.read_file", func(_ context.Context, params map[string]any) (string, error) {
		return rt.readFile(params)
	})
	server.RegisterToolHandler("filesystem.write_file", func(_ context.Context, params map[string]any) (string, error) {
		return rt.writeFile(params)
	})
	server.RegisterToolHandler("filesystem.append_file", func(_ context.Context, params map[string]any) (string, error) {
		return rt.appendFile(params)
	})
	server.RegisterToolHandler("filesystem.edit_file", func(_ context.Context, params map[string]any) (string, error) {
		return rt.editFile(params)
	})
	server.RegisterToolHandler("filesystem.stat", func(_ context.Context, params map[string]any) (string, error) {
		return rt.statPath(params)
	})
	server.RegisterToolHandler("filesystem.mkdir", func(_ context.Context, params map[string]any) (string, error) {
		return rt.mkdir(params)
	})
	server.RegisterToolHandler("filesystem.delete", func(_ context.Context, params map[string]any) (string, error) {
		return rt.deletePath(params)
	})
	server.RegisterToolHandler("filesystem.move", func(_ context.Context, params map[string]any) (string, error) {
		return rt.movePath(params)
	})
	server.RegisterToolHandler("filesystem.copy", func(_ context.Context, params map[string]any) (string, error) {
		return rt.copyPath(params)
	})
	server.RegisterToolHandler("filesystem.glob", func(_ context.Context, params map[string]any) (string, error) {
		return rt.globPaths(params)
	})
	return server
}

func toolDef(name, desc string, properties map[string]any, required []string) mcplite.ToolDefinition {
	return mcplite.ToolDefinition{
		Name:        name,
		Description: desc,
		Params: map[string]any{
			"type":       "object",
			"properties": properties,
			"required":   required,
		},
	}
}

func stringProp(desc string) map[string]any {
	return map[string]any{"type": "string", "description": desc}
}
func intProp(desc string) map[string]any {
	return map[string]any{"type": "integer", "description": desc}
}
func boolProp(desc string) map[string]any {
	return map[string]any{"type": "boolean", "description": desc}
}

func (rt *filesystemRuntime) resolvePath(raw string, allowMissing bool) (string, string, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return "", "", errors.New("path is required")
	}
	candidate := raw
	if !filepath.IsAbs(candidate) {
		candidate = filepath.Join(rt.root, candidate)
	}
	abs, err := filepath.Abs(candidate)
	if err != nil {
		return "", "", err
	}
	abs = filepath.Clean(abs)
	if !allowMissing {
		if _, err := os.Stat(abs); err != nil {
			return "", "", err
		}
	}
	if err := ensureWithinRoot(rt.root, abs); err != nil {
		return "", "", err
	}
	rel, err := filepath.Rel(rt.root, abs)
	if err != nil {
		return "", "", err
	}
	return abs, filepath.ToSlash(rel), nil
}

func ensureWithinRoot(root, path string) error {
	rel, err := filepath.Rel(root, path)
	if err != nil {
		return err
	}
	if rel == ".." || strings.HasPrefix(rel, ".."+string(os.PathSeparator)) {
		return fmt.Errorf("path escapes root: %s", path)
	}
	return nil
}

func (rt *filesystemRuntime) listDir(params map[string]any) (string, error) {
	abs, rel, err := rt.resolvePath(stringParam(params, "path", ""), false)
	if err != nil {
		return "", err
	}
	st, err := os.Stat(abs)
	if err != nil {
		return "", err
	}
	if !st.IsDir() {
		return "", errors.New("path is not a directory")
	}
	includeHidden := boolParam(params, "include_hidden", false)
	maxEntries := clamp(intParam(params, "max_entries", 200), 1, 1000)
	entries, err := os.ReadDir(abs)
	if err != nil {
		return "", err
	}
	items := make([]map[string]any, 0, maxEntries)
	truncated := false
	for _, entry := range entries {
		name := entry.Name()
		if !includeHidden && strings.HasPrefix(name, ".") {
			continue
		}
		itemType := "file"
		if entry.IsDir() {
			itemType = "dir"
		}
		items = append(items, map[string]any{"name": name, "type": itemType})
		if len(items) >= maxEntries {
			truncated = true
			break
		}
	}
	return marshalAny(map[string]any{"path": rel, "entries": items, "truncated": truncated})
}

func (rt *filesystemRuntime) readFile(params map[string]any) (string, error) {
	abs, rel, err := rt.resolvePath(stringParam(params, "path", ""), false)
	if err != nil {
		return "", err
	}
	st, err := os.Stat(abs)
	if err != nil {
		return "", err
	}
	if st.IsDir() {
		return "", errors.New("path is a directory")
	}
	offset := int64(max(intParam(params, "offset", 0), 0))
	maxBytes := clamp(intParam(params, "max_bytes", defaultMaxBytes), 1, maxBytesHardCap)
	data, err := os.ReadFile(abs)
	if err != nil {
		return "", err
	}
	if offset > int64(len(data)) {
		offset = int64(len(data))
	}
	end := offset + int64(maxBytes)
	if end > int64(len(data)) {
		end = int64(len(data))
	}
	slice := data[offset:end]
	return marshalAny(map[string]any{
		"path":      rel,
		"size":      len(data),
		"offset":    offset,
		"max_bytes": maxBytes,
		"truncated": end < int64(len(data)),
		"content":   string(slice),
	})
}

func (rt *filesystemRuntime) writeFile(params map[string]any) (string, error) {
	path := stringParam(params, "path", "")
	content, ok := params["content"].(string)
	if !ok {
		return "", errors.New("content must be a string")
	}
	abs, rel, err := rt.resolvePath(path, true)
	if err != nil {
		return "", err
	}
	mkdir := boolParam(params, "mkdir", true)
	overwrite := boolParam(params, "overwrite", true)
	if mkdir {
		if err := os.MkdirAll(filepath.Dir(abs), 0o755); err != nil {
			return "", err
		}
	}
	if !overwrite {
		if _, err := os.Stat(abs); err == nil {
			return "", errors.New("target exists and overwrite=false")
		}
	}
	if err := os.WriteFile(abs, []byte(content), 0o644); err != nil {
		return "", err
	}
	return marshalAny(map[string]any{"ok": true, "path": rel, "bytes": len(content)})
}

func (rt *filesystemRuntime) appendFile(params map[string]any) (string, error) {
	path := stringParam(params, "path", "")
	content, ok := params["content"].(string)
	if !ok {
		return "", errors.New("content must be a string")
	}
	abs, rel, err := rt.resolvePath(path, true)
	if err != nil {
		return "", err
	}
	if boolParam(params, "mkdir", true) {
		if err := os.MkdirAll(filepath.Dir(abs), 0o755); err != nil {
			return "", err
		}
	}
	f, err := os.OpenFile(abs, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return "", err
	}
	defer f.Close()
	n, err := f.WriteString(content)
	if err != nil {
		return "", err
	}
	return marshalAny(map[string]any{"ok": true, "path": rel, "bytes_appended": n})
}

func (rt *filesystemRuntime) editFile(params map[string]any) (string, error) {
	abs, rel, err := rt.resolvePath(stringParam(params, "path", ""), false)
	if err != nil {
		return "", err
	}
	oldText, ok := params["old_text"].(string)
	if !ok || oldText == "" {
		return "", errors.New("old_text must be a non-empty string")
	}
	newText, ok := params["new_text"].(string)
	if !ok {
		return "", errors.New("new_text must be a string")
	}
	replaceAll := boolParam(params, "replace_all", false)
	data, err := os.ReadFile(abs)
	if err != nil {
		return "", err
	}
	content := string(data)
	count := strings.Count(content, oldText)
	if count == 0 {
		return "", errors.New("old_text not found")
	}
	if count > 1 && !replaceAll {
		return "", fmt.Errorf("old_text occurs %d times; set replace_all=true", count)
	}
	updated := strings.Replace(content, oldText, newText, 1)
	replaced := 1
	if replaceAll {
		updated = strings.ReplaceAll(content, oldText, newText)
		replaced = count
	}
	if err := os.WriteFile(abs, []byte(updated), 0o644); err != nil {
		return "", err
	}
	return marshalAny(map[string]any{"ok": true, "path": rel, "replaced": replaced})
}

func (rt *filesystemRuntime) statPath(params map[string]any) (string, error) {
	abs, rel, err := rt.resolvePath(stringParam(params, "path", ""), false)
	if err != nil {
		return "", err
	}
	st, err := os.Stat(abs)
	if err != nil {
		return "", err
	}
	typ := "file"
	if st.IsDir() {
		typ = "dir"
	}
	return marshalAny(fileStat{
		Path:        rel,
		Type:        typ,
		Size:        st.Size(),
		Mode:        st.Mode().String(),
		ModifiedUTC: st.ModTime().UTC().Format(time.RFC3339),
	})
}

func (rt *filesystemRuntime) mkdir(params map[string]any) (string, error) {
	abs, rel, err := rt.resolvePath(stringParam(params, "path", ""), true)
	if err != nil {
		return "", err
	}
	recursive := boolParam(params, "recursive", true)
	if recursive {
		if err := os.MkdirAll(abs, 0o755); err != nil {
			return "", err
		}
	} else {
		if err := os.Mkdir(abs, 0o755); err != nil {
			return "", err
		}
	}
	return marshalAny(map[string]any{"ok": true, "path": rel})
}

func (rt *filesystemRuntime) deletePath(params map[string]any) (string, error) {
	abs, rel, err := rt.resolvePath(stringParam(params, "path", ""), false)
	if err != nil {
		return "", err
	}
	st, err := os.Stat(abs)
	if err != nil {
		return "", err
	}
	recursive := boolParam(params, "recursive", false)
	if st.IsDir() && !recursive {
		return "", errors.New("path is a directory; set recursive=true")
	}
	if st.IsDir() {
		if err := os.RemoveAll(abs); err != nil {
			return "", err
		}
	} else {
		if err := os.Remove(abs); err != nil {
			return "", err
		}
	}
	return marshalAny(map[string]any{"ok": true, "path": rel})
}

func (rt *filesystemRuntime) movePath(params map[string]any) (string, error) {
	srcAbs, srcRel, err := rt.resolvePath(stringParam(params, "src", ""), false)
	if err != nil {
		return "", err
	}
	dstAbs, dstRel, err := rt.resolvePath(stringParam(params, "dst", ""), true)
	if err != nil {
		return "", err
	}
	if err := os.MkdirAll(filepath.Dir(dstAbs), 0o755); err != nil {
		return "", err
	}
	if err := os.Rename(srcAbs, dstAbs); err != nil {
		return "", err
	}
	return marshalAny(map[string]any{"ok": true, "src": srcRel, "dst": dstRel})
}

func (rt *filesystemRuntime) copyPath(params map[string]any) (string, error) {
	srcAbs, srcRel, err := rt.resolvePath(stringParam(params, "src", ""), false)
	if err != nil {
		return "", err
	}
	dstAbs, dstRel, err := rt.resolvePath(stringParam(params, "dst", ""), true)
	if err != nil {
		return "", err
	}
	if err := copyRecursive(srcAbs, dstAbs); err != nil {
		return "", err
	}
	return marshalAny(map[string]any{"ok": true, "src": srcRel, "dst": dstRel})
}

func copyRecursive(src, dst string) error {
	st, err := os.Stat(src)
	if err != nil {
		return err
	}
	if st.IsDir() {
		if err := os.MkdirAll(dst, st.Mode().Perm()); err != nil {
			return err
		}
		entries, err := os.ReadDir(src)
		if err != nil {
			return err
		}
		for _, e := range entries {
			if err := copyRecursive(filepath.Join(src, e.Name()), filepath.Join(dst, e.Name())); err != nil {
				return err
			}
		}
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
		return err
	}
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	defer out.Close()
	if _, err := io.Copy(out, in); err != nil {
		return err
	}
	return out.Chmod(st.Mode().Perm())
}

func (rt *filesystemRuntime) globPaths(params map[string]any) (string, error) {
	pattern := strings.TrimSpace(stringParam(params, "pattern", ""))
	if pattern == "" {
		return "", errors.New("pattern is required")
	}
	rootRaw := stringParam(params, "root", "")
	root := rt.root
	if strings.TrimSpace(rootRaw) != "" {
		abs, _, err := rt.resolvePath(rootRaw, false)
		if err != nil {
			return "", err
		}
		root = abs
	}
	fullPattern := pattern
	if !filepath.IsAbs(pattern) {
		fullPattern = filepath.Join(root, pattern)
	}
	matches, err := filepath.Glob(fullPattern)
	if err != nil {
		return "", err
	}
	includeHidden := boolParam(params, "include_hidden", false)
	maxResults := clamp(intParam(params, "max_results", 200), 1, 1000)
	items := make([]string, 0, maxResults)
	truncated := false
	for _, match := range matches {
		if err := ensureWithinRoot(rt.root, match); err != nil {
			continue
		}
		rel, err := filepath.Rel(rt.root, match)
		if err != nil {
			continue
		}
		rel = filepath.ToSlash(rel)
		if !includeHidden && hasHiddenSegment(rel) {
			continue
		}
		items = append(items, rel)
		if len(items) >= maxResults {
			truncated = true
			break
		}
	}
	return marshalAny(map[string]any{"root": filepath.ToSlash(root), "pattern": pattern, "matches": items, "truncated": truncated})
}

func hasHiddenSegment(rel string) bool {
	for _, seg := range strings.Split(rel, "/") {
		if strings.HasPrefix(seg, ".") {
			return true
		}
	}
	return false
}

func parseSearchOptions(rt *filesystemRuntime, params map[string]any) (searchOptions, error) {
	query := strings.TrimSpace(stringParam(params, "query", ""))
	if query == "" {
		return searchOptions{}, errors.New("query is required")
	}

	root := rt.root
	if rawRoot := strings.TrimSpace(stringParam(params, "root", "")); rawRoot != "" {
		abs, _, err := rt.resolvePath(rawRoot, false)
		if err != nil {
			return searchOptions{}, err
		}
		root = abs
	}

	maxResults := clamp(intParam(params, "max_results", 50), 1, 500)
	ext := parseExtensions(params["extensions"])

	return searchOptions{
		Root:          root,
		Query:         query,
		MaxResults:    maxResults,
		IncludeHidden: boolParam(params, "include_hidden", false),
		CaseSensitive: boolParam(params, "case_sensitive", false),
		Extensions:    ext,
	}, nil
}

func parseExtensions(raw any) map[string]struct{} {
	out := make(map[string]struct{})
	list, ok := raw.([]any)
	if !ok {
		return out
	}
	for _, item := range list {
		s, ok := item.(string)
		if !ok {
			continue
		}
		s = strings.TrimSpace(strings.ToLower(s))
		if s == "" {
			continue
		}
		if !strings.HasPrefix(s, ".") {
			s = "." + s
		}
		out[s] = struct{}{}
	}
	return out
}

func searchFilesystem(opts searchOptions) (searchResult, error) {
	info, err := os.Stat(opts.Root)
	if err != nil {
		return searchResult{}, fmt.Errorf("stat root: %w", err)
	}
	if !info.IsDir() {
		return searchResult{}, errors.New("root must be a directory")
	}

	query := opts.Query
	if !opts.CaseSensitive {
		query = strings.ToLower(query)
	}

	hits := make([]searchHit, 0, opts.MaxResults)
	truncated := false

	walkErr := filepath.WalkDir(opts.Root, func(path string, d fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return nil
		}

		name := d.Name()
		if name == "." {
			return nil
		}
		if !opts.IncludeHidden && strings.HasPrefix(name, ".") {
			if d.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		if d.IsDir() {
			if _, skip := defaultSkipDirs[name]; skip {
				return filepath.SkipDir
			}
		}

		rel, err := filepath.Rel(opts.Root, path)
		if err != nil {
			return nil
		}
		rel = filepath.ToSlash(rel)
		if rel == "." {
			return nil
		}

		if len(opts.Extensions) > 0 && !d.IsDir() {
			ext := strings.ToLower(filepath.Ext(name))
			if _, ok := opts.Extensions[ext]; !ok {
				return nil
			}
		}

		candidate := rel
		if !opts.CaseSensitive {
			candidate = strings.ToLower(candidate)
		}

		if !strings.Contains(candidate, query) {
			return nil
		}

		hitType := "file"
		if d.IsDir() {
			hitType = "dir"
		}
		hits = append(hits, searchHit{Path: rel, Type: hitType})
		if len(hits) >= opts.MaxResults {
			truncated = true
			return errResultLimitReached
		}
		return nil
	})
	if walkErr != nil && !errors.Is(walkErr, errResultLimitReached) {
		return searchResult{}, walkErr
	}

	return searchResult{Root: opts.Root, Query: opts.Query, Hits: hits, Truncated: truncated}, nil
}

func handleConn(ctx context.Context, conn net.Conn, server *mcplite.Server) {
	defer conn.Close()

	decoder := mcplite.NewDecoder(conn)
	encoder := mcplite.NewEncoder(conn)
	var writeMu sync.Mutex
	var reqWG sync.WaitGroup

	for {
		frame, err := decoder.Next()
		if err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			mcplite.LogEvent("ERROR", "decode frame failed", map[string]any{"service": "filesystem", "error": err.Error()})
			break
		}

		reqWG.Add(1)
		go func(f mcplite.Frame) {
			defer reqWG.Done()
			start := time.Now()
			requestID := mcplite.RequestIDFromFrame(f)
			tool := mcplite.ToolNameFromFrame(f)
			outcome := "ok"

			response, handleErr := server.HandleRequest(ctx, f)
			if handleErr != nil {
				outcome = "error"
				id := frameID(f)
				if id == "" {
					mcplite.LogEvent("WARN", "unsupported frame", map[string]any{"service": "filesystem", "frame": fmt.Sprintf("%T", f)})
					return
				}
				response = mcplite.ErrorResponse{ID: id, Type: mcplite.TypeError, Code: "BAD_REQUEST", Message: handleErr.Error()}
			}

			writeMu.Lock()
			defer writeMu.Unlock()
			if err := encoder.WriteFrame(response); err != nil {
				outcome = "error"
				mcplite.LogEvent("ERROR", "write frame failed", map[string]any{"service": "filesystem", "request_id": requestID, "tool": tool, "error": err.Error()})
				return
			}
			mcplite.LogEvent("INFO", "request handled", map[string]any{"service": "filesystem", "request_id": requestID, "tool": tool, "outcome": outcome, "duration_ms": float64(time.Since(start).Microseconds()) / 1000.0})
		}(frame)
	}

	reqWG.Wait()
}

func frameID(frame mcplite.Frame) string {
	switch v := frame.(type) {
	case mcplite.ToolListRequest:
		return v.ID
	case mcplite.ToolCallRequest:
		return v.ID
	case mcplite.PingRequest:
		return v.ID
	default:
		return ""
	}
}

func marshalAny(v any) (string, error) {
	data, err := json.Marshal(v)
	if err != nil {
		return "", err
	}
	return string(data), nil
}

func stringParam(params map[string]any, key string, fallback string) string {
	raw, ok := params[key]
	if !ok || raw == nil {
		return fallback
	}
	switch v := raw.(type) {
	case string:
		return v
	default:
		return fmt.Sprintf("%v", raw)
	}
}

func intParam(params map[string]any, key string, fallback int) int {
	raw, ok := params[key]
	if !ok || raw == nil {
		return fallback
	}
	switch v := raw.(type) {
	case int:
		return v
	case int64:
		return int(v)
	case float64:
		return int(v)
	case string:
		i, err := strconv.Atoi(v)
		if err != nil {
			return fallback
		}
		return i
	default:
		return fallback
	}
}

func boolParam(params map[string]any, key string, fallback bool) bool {
	raw, ok := params[key]
	if !ok || raw == nil {
		return fallback
	}
	switch v := raw.(type) {
	case bool:
		return v
	case string:
		b, err := strconv.ParseBool(v)
		if err != nil {
			return fallback
		}
		return b
	default:
		return fallback
	}
}

func clamp(v, minV, maxV int) int {
	if v < minV {
		return minV
	}
	if v > maxV {
		return maxV
	}
	return v
}

func max(a, b int) int {
	if a > b {
		return a
	}
	return b
}
