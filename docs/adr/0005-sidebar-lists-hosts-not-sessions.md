# The sidebar lists Hosts, not Sessions

The sidebar always shows every saved Host and Local Shell. Clicking one activates it: Komando shows its Session, or connects if there is none. When a connection drops or is closed, the item goes idle but keeps its Scrollback, and clicking it again reconnects in the same terminal. This replaces the first design, where the sidebar listed open Sessions ("tabs") and Hosts were opened through a picker. System engineers think in servers, not in tabs; having every server one click away removes the open-a-tab-for-this-client step. The idea comes from Arc's browser sidebar.

## Consequences

- Projects stay collapsible groups (not Arc-style "spaces") for now.
- A second Session on the same Host is explicit (Ctrl+click or +). An item with several Sessions becomes a group that can be folded: the item's row shows how many, and each Session gets its own row with its own close button. The item's row has no close button then, since it would not be clear which Session it closes.
- Local Shells can stand alone (top of the sidebar) or belong to a Project. Standalone ones can be hidden from the sidebar in Settings; a hidden or removed one still shows while it has Sessions open, so no running Session becomes unreachable.
