import { createRootRoute, createRoute, createRouter, Outlet } from "@tanstack/react-router"

import { Sidebar } from "@/components/layout/sidebar"
import { Toaster } from "@/components/ui/sonner"
import { KeysPage } from "@/routes/keys"
import { MailPage } from "@/routes/mail"
import { MessagePage } from "@/routes/message"
import { OverviewPage } from "@/routes/overview"
import { WebhooksPage } from "@/routes/webhooks"

/**
 * Routes are declared here rather than generated from the filesystem: there are five of
 * them, and a generated route tree would add a build step and a committed artifact to
 * keep in sync for no gain at this size.
 *
 * The whole app is served under `/_admin/console/`, which is why `basepath` is set. The
 * Resend API is on the same port and owns `/emails/{id}`, so a console route of that
 * shape at the root would collide with the API it exists to show.
 */
const rootRoute = createRootRoute({
  component: () => (
    <div className="flex">
      <Sidebar />
      <Outlet />
      <Toaster position="bottom-right" />
    </div>
  ),
})

const overviewRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: OverviewPage,
})

const mailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/mail",
  component: MailPage,
  // The key filter lives in the URL so a filtered list can be linked and reloaded.
  validateSearch: (search: Record<string, unknown>): { api_key?: string } =>
    typeof search.api_key === "string" && search.api_key !== "" ? { api_key: search.api_key } : {},
})

const messageRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/mail/$id",
  component: MessagePage,
})

const keysRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/keys",
  component: KeysPage,
})

const webhooksRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/webhooks",
  component: WebhooksPage,
})

const routeTree = rootRoute.addChildren([
  overviewRoute,
  mailRoute,
  messageRoute,
  keysRoute,
  webhooksRoute,
])

export const router = createRouter({ routeTree, basepath: "/_admin/console" })

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router
  }
}
