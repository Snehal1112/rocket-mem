import { useEffect, useRef, useState } from "react"
import { Check, Copy } from "lucide-react"

import { Button } from "@/components/ui/button"

export function CopyCodeBlock({ code }: { code: string }) {
  const [copied, setCopied] = useState(false)
  const resetTimer = useRef<ReturnType<typeof setTimeout> | undefined>(
    undefined
  )

  // Drop a pending reset if the block unmounts mid-countdown.
  useEffect(() => () => clearTimeout(resetTimer.current), [])

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code)
    } catch {
      // Clipboard access can be denied or unavailable outside a secure
      // context. The command stays selectable, so fail quietly.
      return
    }
    setCopied(true)
    clearTimeout(resetTimer.current)
    resetTimer.current = setTimeout(() => setCopied(false), 1500)
  }

  return (
    <div className="flex items-start gap-2 rounded-2xl border border-border bg-muted/50 px-4 py-3">
      <pre className="flex-1 overflow-x-auto font-heading text-xs whitespace-pre-wrap text-foreground sm:text-sm">
        {code}
      </pre>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={() => void copy()}
        aria-label="Copy command"
        className="shrink-0"
      >
        {copied ? <Check className="text-primary" /> : <Copy />}
      </Button>
      <span aria-live="polite" className="sr-only">
        {copied ? "Command copied" : ""}
      </span>
    </div>
  )
}
