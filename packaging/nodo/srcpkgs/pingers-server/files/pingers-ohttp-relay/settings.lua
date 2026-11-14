-- pingers-ohttp-relay's settings (relay.toml.in). The gateway addresses are pingers-server's
-- public settings gateway-url and gateway-key-config-url: referencing them is what opens the
-- relay's one loopback port to the gateway.
return {
  api_version = 1,
  settings = {
    {
      name = "response_timeout_seconds",
      type = "int",
      label = "Gateway response timeout",
      help = "Seconds to wait for the gateway's response headers (response_timeout_seconds). "
        .. "Bodies, which for a Meetup's live channel last as long as the Meetup, have no limit.",
      default = 30,
      min = 5,
      max = 300,
    },
  },
}
