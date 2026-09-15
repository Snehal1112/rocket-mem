import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { CopyCodeBlock } from "@/components/landing/copy-code-block"

const DOCKER_COMMAND =
  "docker run --rm -p 6379:6379 -p 6380:6380 ghcr.io/snehal1112/rocket-mem:latest"

const BINARY_COMMAND = `VERSION=v0.1.4
curl -LO https://github.com/Snehal1112/rocket-mem/releases/download/$VERSION/rocket-mem-$VERSION-linux-amd64.tar.gz
curl -LO https://github.com/Snehal1112/rocket-mem/releases/download/$VERSION/rocket-mem-$VERSION-linux-amd64.tar.gz.sha256
sha256sum -c rocket-mem-$VERSION-linux-amd64.tar.gz.sha256
tar -xzf rocket-mem-$VERSION-linux-amd64.tar.gz
./rocket-mem-$VERSION-linux-amd64`

const SOURCE_COMMAND = `git clone https://github.com/Snehal1112/rocket-mem.git
cd rocket-mem
cargo build --release --bin rocket-mem
./target/release/rocket-mem`

export function Quickstart() {
  return (
    <section id="quickstart" className="mx-auto max-w-3xl px-4 py-20 sm:px-6">
      <div className="mb-10 flex flex-col gap-2 text-center">
        <h2 className="font-heading text-3xl font-semibold">Quick start</h2>
        <p className="text-muted-foreground">
          No configuration file and no environment variables required.
        </p>
      </div>
      <Tabs defaultValue="docker">
        <TabsList className="mx-auto">
          <TabsTrigger value="docker">Docker</TabsTrigger>
          <TabsTrigger value="binary">Binary download</TabsTrigger>
          <TabsTrigger value="source">Build from source</TabsTrigger>
        </TabsList>
        <TabsContent value="docker">
          <CopyCodeBlock code={DOCKER_COMMAND} />
        </TabsContent>
        <TabsContent value="binary">
          <CopyCodeBlock code={BINARY_COMMAND} />
        </TabsContent>
        <TabsContent value="source">
          <CopyCodeBlock code={SOURCE_COMMAND} />
        </TabsContent>
      </Tabs>
    </section>
  )
}
