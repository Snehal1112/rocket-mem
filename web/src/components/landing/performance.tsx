import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from "recharts"

import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart"

type WorkloadRow = {
  workload: string
  "redis-server": number
  "rocket-mem": number
}

// Both req/s columns are transcribed from the README's Performance table.
// The eight rows are split by pipelining depth because the two regimes sit an
// order of magnitude apart, and one shared axis would flatten the unpipelined
// four into slivers next to the pipelined ones.
const NO_PIPELINE: WorkloadRow[] = [
  { workload: "SET 3B", "redis-server": 78247, "rocket-mem": 90662 },
  { workload: "SET 1KB", "redis-server": 96339, "rocket-mem": 86655 },
  { workload: "GET 3B", "redis-server": 105597, "rocket-mem": 98039 },
  { workload: "GET 1KB", "redis-server": 99010, "rocket-mem": 97943 },
]

const PIPELINED: WorkloadRow[] = [
  { workload: "SET 3B", "redis-server": 763359, "rocket-mem": 729927 },
  { workload: "SET 1KB", "redis-server": 438596, "rocket-mem": 500000 },
  { workload: "GET 3B", "redis-server": 1449275, "rocket-mem": 1176471 },
  { workload: "GET 1KB", "redis-server": 746269, "rocket-mem": 763359 },
]

// The baseline stays neutral and the subject carries the brand color, so the
// two series separate by saturation as well as by lightness. --chart-1 is
// deliberately not used: at L 0.872 it is near-invisible on the light theme's
// white ground, and it holds the same value in both themes.
const chartConfig = {
  "redis-server": { label: "redis-server", color: "var(--chart-2)" },
  "rocket-mem": { label: "rocket-mem", color: "var(--primary)" },
} satisfies ChartConfig

const COMPACT_REQS = new Intl.NumberFormat("en-US", {
  notation: "compact",
  maximumFractionDigits: 1,
})

function WorkloadChart({ data }: { data: WorkloadRow[] }) {
  return (
    <ChartContainer
      config={chartConfig}
      className="aspect-auto h-52 w-full [&_.recharts-cartesian-axis-tick_text]:font-heading"
    >
      <BarChart
        accessibilityLayer
        data={data}
        layout="vertical"
        barGap={4}
        margin={{ top: 4, right: 16, bottom: 0, left: 0 }}
      >
        <CartesianGrid horizontal={false} />
        <XAxis
          type="number"
          tickLine={false}
          axisLine={false}
          tickMargin={6}
          tickFormatter={(value: number) => COMPACT_REQS.format(value)}
        />
        <YAxis
          type="category"
          dataKey="workload"
          width={68}
          tickLine={false}
          axisLine={false}
          tickMargin={6}
        />
        <ChartTooltip content={<ChartTooltipContent />} />
        {/* Bars grow rightwards, so only the leading end is rounded. */}
        <Bar
          dataKey="redis-server"
          fill="var(--color-redis-server)"
          barSize={13}
          radius={[0, 3, 3, 0]}
        />
        <Bar
          dataKey="rocket-mem"
          fill="var(--color-rocket-mem)"
          barSize={13}
          radius={[0, 3, 3, 0]}
        />
      </BarChart>
    </ChartContainer>
  )
}

function WorkloadPanel({
  title,
  detail,
  data,
}: {
  title: string
  detail: string
  data: WorkloadRow[]
}) {
  return (
    <div>
      <div className="mb-4 border-t border-border pt-3">
        <p className="font-heading text-sm font-semibold">{title}</p>
        <p className="text-xs text-muted-foreground">{detail}</p>
      </div>
      <WorkloadChart data={data} />
    </div>
  )
}

function Legend() {
  return (
    <div className="mb-10 flex items-center justify-center gap-6 text-sm text-muted-foreground">
      {Object.entries(chartConfig).map(([key, series]) => (
        <span key={key} className="flex items-center gap-2">
          <span
            aria-hidden="true"
            className="size-2.5 rounded-[2px]"
            style={{ backgroundColor: series.color }}
          />
          {series.label}
        </span>
      ))}
    </div>
  )
}

// The charts are SVG, so the same eight rows are repeated as a table for
// screen readers and for anyone who wants the figures rather than the shape.
// The wrapper carries sr-only, not the table: auto table layout ignores the
// utility's 1px width and would otherwise widen the page on a phone.
function DataTable() {
  return (
    <div className="sr-only">
      <table>
        <caption>
          Throughput in requests per second, redis-server against rocket-mem.
        </caption>
        <thead>
          <tr>
            <th>Workload</th>
            <th>redis-server</th>
            <th>rocket-mem</th>
          </tr>
        </thead>
        <tbody>
          {[
            { regime: "no pipelining", rows: NO_PIPELINE },
            { regime: "pipelined 16 deep", rows: PIPELINED },
          ].map((group) =>
            group.rows.map((row) => (
              <tr key={`${group.regime} ${row.workload}`}>
                <th scope="row">
                  {row.workload}, {group.regime}
                </th>
                <td>{row["redis-server"].toLocaleString()}</td>
                <td>{row["rocket-mem"].toLocaleString()}</td>
              </tr>
            ))
          )}
        </tbody>
      </table>
    </div>
  )
}

export function Performance() {
  return (
    <section id="performance" className="mx-auto max-w-4xl px-4 py-20 sm:px-6">
      <div className="mb-8 flex flex-col items-center gap-3 text-center">
        <h2 className="font-heading text-3xl font-semibold">Performance</h2>
        <p className="max-w-xl font-heading text-lg font-semibold sm:text-xl">
          0.86x–1.23x of Redis, faster on 3 of 8 measured workloads
        </p>
        <p className="max-w-2xl text-sm leading-relaxed text-muted-foreground">
          <code>redis-benchmark -t set,get -n 100000 -c 50 -r 100000</code>,
          median of three sweeps against redis-server 8.10.1 on the same host,
          with matching durability (<code>appendonly yes</code>,{" "}
          <code>appendfsync everysec</code>) on both servers.
        </p>
      </div>

      <Legend />

      {/* Each panel carries its own scale, so bar lengths compare within a
          panel and never across the two. */}
      <div className="grid gap-x-10 gap-y-10 lg:grid-cols-2">
        <WorkloadPanel
          title="No pipelining"
          detail="One command per round trip, 50 connections."
          data={NO_PIPELINE}
        />
        <WorkloadPanel
          title="Pipelined, 16 deep"
          detail="Sixteen commands in flight per connection (-P 16)."
          data={PIPELINED}
        />
      </div>

      <DataTable />

      <div className="mt-10 grid gap-6 sm:grid-cols-2">
        <p className="border-l-2 border-primary/40 pl-4 text-sm leading-relaxed text-muted-foreground">
          rocket-mem leads on 3B <code>SET</code> without pipelining, and on
          both 1KB workloads when pipelined. Redis's widest margin is pipelined
          3B <code>GET</code>. Pipelining lifts both servers by roughly an order
          of magnitude, which is why the two panels are scaled separately.
        </p>
        <p className="border-l-2 border-primary/40 pl-4 text-sm leading-relaxed text-muted-foreground">
          Medians from a single, noisy host, measured 2026-09-08 — run-to-run
          spread is wide, so no ratio here should be read to two significant
          figures, and these numbers are a snapshot of that date rather than a
          live measurement.
        </p>
      </div>
    </section>
  )
}
