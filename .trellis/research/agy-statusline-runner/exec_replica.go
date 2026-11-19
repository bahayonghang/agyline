//go:build ignore

// Replica of the exec call shape read from store.(*StatusLineRunner).run in agy 1.5.9
// (windows/amd64): exec.CommandContext(ctx, "cmd", "/c", command), Stdin = bytes.Reader,
// Stdout/Stderr = bytes.Buffer, Env = os.Environ(), and no Dir, SysProcAttr, WaitDelay or Cancel.
//
// This is NOT agy. It only shows what the Go standard library does with that call shape on
// Windows: how the command string is quoted for cmd.exe, and what a cancelled or timed-out
// context does to a process that cmd.exe started.
//
// Usage:
//
//	go run exec_replica.go quote   <command>...
//	go run exec_replica.go cancel  <delay>  <command>   # cancel the context after <delay>
//	go run exec_replica.go timeout <limit>  <command>   # context.WithTimeout(<limit>)
package main

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"
)

func run(ctx context.Context, command string, payload []byte) (string, string, error, time.Duration) {
	cmd := exec.CommandContext(ctx, "cmd", "/c", command)
	cmd.Stdin = bytes.NewReader(payload)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	cmd.Env = os.Environ()
	start := time.Now()
	err := cmd.Run()
	return stdout.String(), stderr.String(), err, time.Since(start)
}

func report(ctx context.Context, command string) {
	out, errOut, err, d := run(ctx, command, []byte(`{"x":1}`))
	fmt.Printf("stdout  : %q\nstderr  : %q\nerr     : %v\nctx.Err : %v\nRun returned after: %v\n\n",
		out, strings.TrimSpace(errOut), err, ctx.Err(), d.Round(time.Millisecond))
}

func main() {
	if len(os.Args) < 3 {
		fmt.Fprintln(os.Stderr, "usage: exec_replica quote <command>... | cancel <delay> <command> | timeout <limit> <command>")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "quote":
		for _, command := range os.Args[2:] {
			fmt.Printf("command : %s\nargv    : %q\n", command, exec.Command("cmd", "/c", command).Args)
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			report(ctx, command)
			cancel()
		}
	case "cancel":
		delay, _ := time.ParseDuration(os.Args[2])
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		time.AfterFunc(delay, cancel)
		report(ctx, os.Args[3])
	case "timeout":
		limit, _ := time.ParseDuration(os.Args[2])
		ctx, cancel := context.WithTimeout(context.Background(), limit)
		defer cancel()
		report(ctx, os.Args[3])
	}
}
