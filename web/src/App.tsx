function App() {
  return (
    <div className="flex min-h-svh flex-col items-center justify-center gap-4 p-8">
      <h1 className="font-heading text-3xl font-semibold">rocket-mem</h1>
      <p className="max-w-prose text-center text-muted-foreground">
        A Redis-compatible in-memory data store, written from scratch in Rust.
      </p>
    </div>
  )
}

export default App
