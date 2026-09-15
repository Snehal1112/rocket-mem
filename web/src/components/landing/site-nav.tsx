import { useState, type SVGProps } from "react"
import { Menu } from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet"
import { useTheme } from "@/components/theme-provider"

const NAV_LINKS = [
  { href: "#why", label: "Why" },
  { href: "#features", label: "Features" },
  { href: "#architecture", label: "Architecture" },
  { href: "#performance", label: "Performance" },
  { href: "#security", label: "Security" },
  { href: "#quickstart", label: "Quick Start" },
]

const THEME_SEQUENCE = ["light", "dark", "system"] as const

const GITHUB_URL = "https://github.com/Snehal1112/rocket-mem"

// lucide-react dropped brand/logo icons (including Github) as of 1.4x, so
// the mark is inlined here instead of imported.
function GithubIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true" {...props}>
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8z" />
    </svg>
  )
}

function ThemeToggle() {
  const { theme, setTheme } = useTheme()

  const cycleTheme = () => {
    const currentIndex = THEME_SEQUENCE.indexOf(theme)
    const nextTheme =
      THEME_SEQUENCE[(currentIndex + 1) % THEME_SEQUENCE.length]
    setTheme(nextTheme)
  }

  return (
    <Button variant="outline" size="sm" onClick={cycleTheme}>
      Theme: {theme}
    </Button>
  )
}

function GitHubButton() {
  return (
    <Button
      variant="outline"
      size="sm"
      render={<a href={GITHUB_URL} target="_blank" rel="noreferrer" />}
    >
      <GithubIcon />
      GitHub
    </Button>
  )
}

function NavLinks({ onNavigate }: { onNavigate?: () => void }) {
  return (
    <>
      {NAV_LINKS.map((link) => (
        <a
          key={link.href}
          href={link.href}
          onClick={onNavigate}
          className="text-sm font-medium text-muted-foreground transition-colors hover:text-foreground"
        >
          {link.label}
        </a>
      ))}
    </>
  )
}

export function SiteNav() {
  const [mobileOpen, setMobileOpen] = useState(false)

  return (
    <header className="sticky top-0 z-40 border-b border-border bg-background/80 backdrop-blur-sm">
      <div className="mx-auto flex h-16 max-w-6xl items-center justify-between gap-4 px-4 sm:px-6">
        <a href="#top" className="font-heading text-lg font-semibold">
          rocket-mem
        </a>

        <nav className="hidden items-center gap-6 md:flex">
          <NavLinks />
        </nav>

        <div className="hidden items-center gap-2 md:flex">
          <ThemeToggle />
          <GitHubButton />
        </div>

        <div className="flex items-center gap-2 md:hidden">
          <ThemeToggle />
          <Sheet open={mobileOpen} onOpenChange={setMobileOpen}>
            <SheetTrigger render={<Button variant="outline" size="icon" />}>
              <Menu />
              <span className="sr-only">Open menu</span>
            </SheetTrigger>
            <SheetContent side="right">
              <SheetHeader>
                <SheetTitle>rocket-mem</SheetTitle>
              </SheetHeader>
              <nav className="flex flex-col gap-4 px-6">
                <NavLinks onNavigate={() => setMobileOpen(false)} />
                <GitHubButton />
              </nav>
            </SheetContent>
          </Sheet>
        </div>
      </div>
    </header>
  )
}
