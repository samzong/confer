def all_seats: [.rooms[]?.seats[]?];

def test_like:
  ((.workspace // "") | contains("/.local/")) or
  ((.name // "") | ascii_downcase | test("(^|[-_])(smoke|e2e|proof|test|protocol)([-_]|$)"));

def mentions_read_only:
  ((.instructions // "") | ascii_downcase) as $text
  | ($text | contains("read-only")) or ($text | contains("do not edit"));

{
  schema_version,
  rooms: {
    total: (.rooms | length),
    test_like: ([.rooms[] | select(test_like)] | length),
    other: ([.rooms[] | select(test_like | not)] | length)
  },
  seats: {
    total: (all_seats | length),
    with_native_session: ([all_seats[] | select(.native_session_id != null)] | length),
    with_instructions: ([all_seats[] | select(.instructions != null and (.instructions | length > 0))] | length),
    mentioning_read_only: ([all_seats[] | select(mentions_read_only)] | length),
    without_requested_model: ([all_seats[] | select(.model == null)] | length),
    without_requested_reasoning_effort: ([all_seats[] | select(.reasoning_effort == null)] | length)
  },
  agents: (
    [all_seats[] | .agent]
    | sort
    | group_by(.)
    | map({agent: .[0], seats: length})
  ),
  workspaces: (
    .rooms
    | sort_by(.workspace)
    | group_by(.workspace)
    | map({
        workspace: .[0].workspace,
        rooms: length,
        seats: ([.[]?.seats[]?] | length),
        test_like: ([.[] | select(test_like)] | length)
      })
  ),
  room_index: [
    .rooms[]
    | {
        id,
        name,
        workspace,
        created_at,
        updated_at,
        test_like: test_like,
        seats: (.seats | length),
        agents: [.seats[]?.agent],
        started_seats: ([.seats[]? | select(.native_session_id != null)] | length),
        unpinned_model_seats: ([.seats[]? | select(.model == null)] | length),
        unpinned_reasoning_seats: ([.seats[]? | select(.reasoning_effort == null)] | length)
      }
  ]
}
