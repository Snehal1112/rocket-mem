import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
        <TerminalDemo />
      </main>
      <SiteFooter />
    </>
  )
}

export default App
