import { useEffect, useRef, useState } from "react"
import { Check, Copy } from "lucide-react"

import { GithubIcon } from "@/components/landing/github-icon"
import { Button } from "@/components/ui/button"

const GITHUB_URL = "https://github.com/Snehal1112/rocket-mem"
const DOCKER_COMMAND =
  "docker run --rm -p 6379:6379 -p 6380:6380 ghcr.io/snehal1112/rocket-mem:latest"

function DockerCommand() {
  const [copied, setCopied] = useState(false)
  const resetTimer = useRef<ReturnType<typeof setTimeout> | undefined>(
    undefined
  )

  // Drop a pending reset if the section unmounts mid-countdown.
  useEffect(() => () => clearTimeout(resetTimer.current), [])

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(DOCKER_COMMAND)
    } catch {
      // Clipboard access can be denied or unavailable outside a secure
      // context. The command stays selectable, so fail quietly.
      return
    }
    setCopied(true)
    clearTimeout(resetTimer.current)
    resetTimer.current = setTimeout(() => setCopied(false), 1500)
  }

  // The box sizes to the command itself once the row has room for it, so
  // the command only scrolls horizontally on narrow screens.
  return (
    <div className="flex w-full min-w-0 items-center gap-2 rounded-xl border border-border bg-muted/40 py-2 pl-3 pr-2 lg:w-auto">
      {/* The prompt is decoration, so keep it out of a drag-selected copy. */}
      <span
        aria-hidden="true"
        className="select-none font-heading text-sm text-muted-foreground"
      >
        $
      </span>
      <code className="min-w-0 flex-1 overflow-x-auto whitespace-pre font-heading text-xs text-foreground sm:text-sm">
        {DOCKER_COMMAND}
      </code>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={() => void copy()}
        aria-label="Copy Docker command"
      >
        {copied ? <Check className="text-primary" /> : <Copy />}
      </Button>
      <span aria-live="polite" className="sr-only">
        {copied ? "Docker command copied" : ""}
      </span>
    </div>
  )
}

export function Hero() {
  return (
    <section className="mx-auto max-w-6xl px-4 pb-16 pt-20 sm:px-6 sm:pb-24 sm:pt-28">
      <h1 className="max-w-[20ch] text-balance font-heading text-3xl font-semibold leading-[1.1] tracking-tight sm:text-5xl lg:text-6xl">
        A Redis-compatible store, built from scratch in Rust
      </h1>

      <p className="mt-6 max-w-[60ch] text-pretty text-base leading-relaxed text-muted-foreground sm:text-lg">
        rocket-mem speaks RESP2 and RESP3. Point{" "}
        <code className="font-heading text-[0.9em] text-foreground">
          redis-cli
        </code>
        ,{" "}
        <code className="font-heading text-[0.9em] text-foreground">
          redis-py
        </code>
        ,{" "}
        <code className="font-heading text-[0.9em] text-foreground">
          ioredis
        </code>
        , or{" "}
        <code className="font-heading text-[0.9em] text-foreground">
          go-redis
        </code>{" "}
        at it and it just works — no code changes.
      </p>

      <div className="mt-10 flex flex-col items-start gap-4 lg:flex-row lg:items-center">
        <Button
          size="lg"
          render={<a href={GITHUB_URL} target="_blank" rel="noreferrer" />}
        >
          <GithubIcon />
          View on GitHub
        </Button>
        <DockerCommand />
      </div>
    </section>
  )
}
