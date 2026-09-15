import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
      </main>
      <SiteFooter />
    </>
  )
}

export default App
