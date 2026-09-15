import { useState } from "react"
import { Check, Copy } from "lucide-react"

import { Button } from "@/components/ui/button"

export function CopyCodeBlock({ code }: { code: string }) {
  const [copied, setCopied] = useState(false)

  const copy = async () => {
    await navigator.clipboard.writeText(code)
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }

  return (
    <div className="flex items-start gap-2 rounded-2xl border border-border bg-muted/50 px-4 py-3">
      <pre className="flex-1 overflow-x-auto whitespace-pre-wrap font-heading text-xs text-foreground sm:text-sm">
        {code}
      </pre>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={copy}
        aria-label="Copy command"
        className="shrink-0"
      >
        {copied ? <Check className="text-primary" /> : <Copy />}
      </Button>
    </div>
  )
}
