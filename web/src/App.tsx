import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { useTheme } from "@/components/theme-provider"

const THEME_SEQUENCE = ["light", "dark", "system"] as const

function App() {
  const { theme, setTheme } = useTheme()

  const cycleTheme = () => {
    const currentIndex = THEME_SEQUENCE.indexOf(theme)
    const nextTheme = THEME_SEQUENCE[(currentIndex + 1) % THEME_SEQUENCE.length]
    setTheme(nextTheme)
  }

  return (
    <div className="flex min-h-svh flex-col items-center justify-center gap-4 p-8">
      <Button variant="outline" onClick={cycleTheme}>
        Theme: {theme}
      </Button>
      <Card className="max-w-md">
        <CardHeader>
          <CardTitle>rocket-mem</CardTitle>
          <CardDescription>
            A Redis-compatible in-memory data store, written from scratch in
            Rust.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <p className="text-sm text-muted-foreground">
            Vendored UI primitives and theming are wired up. Later plans
            replace this sanity check with the real page sections.
          </p>
        </CardContent>
      </Card>
    </div>
  )
}

export default App
