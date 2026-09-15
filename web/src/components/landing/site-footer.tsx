const GITHUB_URL = "https://github.com/Snehal1112/rocket-mem"
const GETTING_STARTED_URL = `${GITHUB_URL}/blob/main/docs/getting-started.md`
const LICENSE_URL = `${GITHUB_URL}/blob/main/LICENSE`

const FOOTER_LINKS = [
  { href: GITHUB_URL, label: "GitHub" },
  { href: GETTING_STARTED_URL, label: "Getting started" },
  { href: LICENSE_URL, label: "License (MIT)" },
]

export function SiteFooter() {
  return (
    <footer className="border-t border-border">
      <div className="mx-auto flex max-w-6xl flex-col gap-4 px-4 py-10 sm:px-6">
        <div className="flex flex-wrap items-center justify-between gap-4">
          <span className="font-heading text-sm font-medium">
            rocket-mem
          </span>
          <nav className="flex flex-wrap gap-6">
            {FOOTER_LINKS.map((link) => (
              <a
                key={link.href}
                href={link.href}
                target="_blank"
                rel="noreferrer"
                className="text-sm text-muted-foreground transition-colors hover:text-foreground"
              >
                {link.label}
              </a>
            ))}
          </nav>
        </div>
        <p className="text-xs text-muted-foreground">
          No automated failover, no live resharding — read the README's
          Limitations section before deploying.
        </p>
      </div>
    </footer>
  )
}
