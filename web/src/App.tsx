import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"
import { Why } from "@/components/landing/why"
import { Features } from "@/components/landing/features"
import { Architecture } from "@/components/landing/architecture"
import { Performance } from "@/components/landing/performance"
import { SecurityObservability } from "@/components/landing/security-observability"
import { CommandCoverage } from "@/components/landing/command-coverage"
import { Quickstart } from "@/components/landing/quickstart"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
        <TerminalDemo />
        <Why />
        <Features />
        <Architecture />
        <Performance />
        <SecurityObservability />
        <CommandCoverage />
        <Quickstart />
      </main>
      <SiteFooter />
    </>
  )
}

export default App
