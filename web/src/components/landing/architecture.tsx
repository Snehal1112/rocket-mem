// Layer names and their contents are transcribed from the README's
// Architecture diagram; the two notes below the diagram are its prose.
const LAYERS = [
  {
    name: "Protocol layer",
    responsibilities: ["RESP2/RESP3", "RMP"],
    showShards: false,
  },
  {
    name: "Command dispatcher",
    responsibilities: ["routing", "arg checks", "auth", "cluster", "AOF"],
    showShards: false,
  },
  {
    name: "Storage engine",
    responsibilities: ["data structures", "expiry", "persistence"],
    showShards: true,
  },
] as const

// One segment per keyspace shard, matching the engine's fixed 16.
const SHARDS = Array.from({ length: 16 }, (_, index) => `shard-${index}`)

export function Architecture() {
  return (
    <section id="architecture" className="mx-auto max-w-4xl px-4 py-20 sm:px-6">
      <div className="mb-10 flex flex-col gap-2 text-center">
        <h2 className="font-heading text-3xl font-semibold">Architecture</h2>
        <p className="text-muted-foreground">
          Three layers, with a strict rule: the storage engine knows nothing
          about any wire protocol.
        </p>
      </div>

      {/* One slab, three bands. The layers share edges instead of floating
          apart, and a single spine runs the full height, so the diagram
          reads as one path down rather than three separate boxes. */}
      <div className="divide-y divide-border overflow-hidden rounded-4xl bg-card text-card-foreground shadow-md ring-1 ring-foreground/5 dark:ring-foreground/10">
        {LAYERS.map((layer) => (
          <div key={layer.name} className="flex gap-4 sm:gap-6">
            <div aria-hidden="true" className="relative w-8 shrink-0 sm:w-10">
              <span className="absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-primary/30" />
              {/* The node sits on the layer name's own line, and its ring
                  punches a hole in the spine running behind it. */}
              <span className="absolute top-[1.75rem] left-1/2 size-2 -translate-x-1/2 rounded-full bg-primary ring-4 ring-card sm:top-[2rem]" />
            </div>

            <div className="grid flex-1 gap-y-2 py-5 pr-5 sm:grid-cols-[11rem_1fr] sm:gap-x-6 sm:py-6 sm:pr-7">
              <p className="font-heading text-base font-semibold tracking-tight">
                {layer.name}
              </p>
              <div className="flex flex-col gap-4">
                <div className="flex flex-wrap gap-x-6 gap-y-1 text-sm leading-6 text-muted-foreground">
                  {layer.responsibilities.map((responsibility) => (
                    <span key={responsibility}>{responsibility}</span>
                  ))}
                </div>

                {layer.showShards && (
                  <div>
                    {/* The same move as the slab above, one scale down: one
                        keyspace divided, not sixteen loose pieces. */}
                    <div
                      aria-hidden="true"
                      className="flex h-7 overflow-hidden rounded-md bg-primary/10 ring-1 ring-primary/30"
                    >
                      {SHARDS.map((shard) => (
                        <span
                          key={shard}
                          className="flex-1 border-l border-primary/25 first:border-l-0"
                        />
                      ))}
                    </div>
                    <p className="mt-2 text-xs text-muted-foreground">
                      The keyspace, split into 16 shards, each behind its own
                      lock.
                    </p>
                  </div>
                )}
              </div>
            </div>
          </div>
        ))}
      </div>

      <div className="mt-8 grid gap-6 sm:grid-cols-2">
        <p className="border-l-2 border-primary/40 pl-4 text-sm leading-relaxed text-muted-foreground">
          That separation is what let RMP be added on top of the existing
          dispatcher without a single change to engine code — both protocols
          build the same command shape and call the same function.
        </p>
        <p className="border-l-2 border-primary/40 pl-4 text-sm leading-relaxed text-muted-foreground">
          One Tokio task per connection, and any task can reach any key by
          taking that key's shard lock — so requests against different keys run
          on different cores at the same instant.
        </p>
      </div>
    </section>
  )
}
