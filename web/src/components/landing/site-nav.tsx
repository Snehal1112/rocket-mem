import { useState } from "react"
import { Menu } from "lucide-react"

import { GithubIcon } from "@/components/landing/github-icon"
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
