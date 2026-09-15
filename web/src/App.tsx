import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"
import { Why } from "@/components/landing/why"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
        <TerminalDemo />
        <Why />
      </main>
      <SiteFooter />
    </>
  )
}

export default App
