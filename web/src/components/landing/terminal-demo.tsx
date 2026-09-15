import { Card } from "@/components/ui/card"

// Transcribed verbatim from the README's Quick start section.
const EXCHANGES = [
  { command: "redis-cli -p 6379 SET user:1 alice", output: "OK" },
  { command: "redis-cli -p 6379 GET user:1", output: '"alice"' },
] as const

function Prompt() {
  // The prompt is decoration, so keep it out of a drag-selected copy.
  return (
    <span aria-hidden="true" className="text-primary select-none">
      $
    </span>
  )
}

export function TerminalDemo() {
  return (
    <section
      aria-label="Example redis-cli session"
      className="mx-auto max-w-6xl px-4 pb-16 sm:px-6 sm:pb-24"
    >
      {/* A nested dark scope, so the card resolves the project's own dark
          tokens in either page theme and always reads as a terminal. */}
      <div className="dark max-w-2xl" style={{ colorScheme: "dark" }}>
        <Card className="p-0 font-heading text-xs leading-relaxed sm:text-sm">
          <div className="overflow-x-auto">
            <div className="min-w-max space-y-4 px-5 py-5 sm:px-6">
              {EXCHANGES.map((exchange) => (
                <div key={exchange.command} className="space-y-1">
                  <div className="flex gap-2">
                    <Prompt />
                    <code className="whitespace-pre text-card-foreground">
                      {exchange.command}
                    </code>
                  </div>
                  <div className="text-muted-foreground">{exchange.output}</div>
                </div>
              ))}

              {/* The session at rest: a prompt and a still cursor. The card is
                  a snapshot, so nothing here animates. */}
              <div className="flex items-center gap-2">
                <Prompt />
                <span
                  aria-hidden="true"
                  className="inline-block h-[1.05em] w-[0.6em] bg-primary/60"
                />
              </div>
            </div>
          </div>
        </Card>
      </div>
    </section>
  )
}
