import { Link } from "@tanstack/react-router"
import { Inbox, KeyRound, LayoutDashboard, Webhook } from "lucide-react"

import { useAdmin } from "@/hooks/use-admin"
import { admin } from "@/lib/admin"
import { cn } from "@/lib/utils"

/** The instance itself: the first thing to check when nothing is working. */
function InstanceStatus() {
  const { data, error } = useAdmin("health", admin.health, { pollMs: 5000 })

  const reachable = error === undefined && data !== undefined

  return (
    <div className="px-4 pt-5 pb-6">
      <div className="font-medium text-[15px] tracking-tight">mock-resend</div>
      <div className="mt-1.5 flex items-center gap-1.5 text-[11.5px] text-muted-foreground">
        <span
          aria-hidden="true"
          className={cn(
            "size-1.5 shrink-0 rounded-full",
            reachable ? "bg-foreground/70" : "bg-muted-foreground/40",
          )}
        />
        {/* No version. The admin API reports its own, which is the server's, and that is
            not the version anyone looking at this cares about — it says nothing about
            the code under test. The one fact worth the space is the one that surprises
            people. The engine's clock is on the hover, because it can be hours ahead
            after `/_admin/clock` and that *does* explain things. */}
        {reachable ? (
          <span className="truncate" title={`Engine time: ${data.now}`}>
            no mail is delivered
          </span>
        ) : (
          <span>not answering</span>
        )}
      </div>
    </div>
  )
}

type NavItem = {
  to: string
  label: string
  icon: typeof Inbox
}

/**
 * Four destinations, and nothing else.
 *
 * The key list is not here. Opening a key would highlight both `Keys` and the key —
 * two selected things in one nav, for one location — and the keys are the content of
 * the Keys page anyway.
 */
const NAV: NavItem[] = [
  { to: "/", label: "Overview", icon: LayoutDashboard },
  { to: "/mail", label: "Mail", icon: Inbox },
  { to: "/keys", label: "API keys", icon: KeyRound },
  { to: "/webhooks", label: "Webhooks", icon: Webhook },
]

export function Sidebar() {
  return (
    <aside className="flex h-svh w-[210px] shrink-0 flex-col border-border/50 border-r">
      <InstanceStatus />

      <nav className="flex-1 space-y-0.5 px-2.5">
        {NAV.map((item) => {
          const Icon = item.icon

          return (
            <Link
              activeOptions={{ exact: item.to === "/" }}
              activeProps={{ "data-active": "true" }}
              className={cn(
                "flex items-center gap-2.5 rounded-md px-2.5 py-[7px] text-[13.5px]",
                "text-muted-foreground transition-colors",
                "hover:bg-accent/60 hover:text-foreground",
                "data-[active=true]:bg-accent data-[active=true]:text-foreground",
              )}
              key={item.to}
              to={item.to}
            >
              <Icon aria-hidden="true" className="size-[15px] shrink-0" strokeWidth={1.75} />
              {item.label}
            </Link>
          )
        })}
      </nav>

      <div className="px-4 py-3.5">
        <a
          className="text-[11px] text-muted-foreground/50 transition-colors hover:text-muted-foreground"
          href="https://rapture.sh"
          rel="noreferrer"
          target="_blank"
        >
          by rapture.sh
        </a>
      </div>
    </aside>
  )
}
