-- pingers-server's settings. Each help text names the server.toml field it fills
-- (server.toml.in). Addresses here are deployment values: example.org stands for your own
-- domain, and nothing here ships with a real host name.
--
-- The two OHTTP gateway keys generate themselves on install (any 32 random bytes are a valid
-- X25519 key), so no key is made off the device. Rotation is yearly and two-slot: serve the
-- other slot (ohttp_served_key_id), and once no supported app build pins the old slot, Reset it
-- to make a new one (docs/deploy-nodo.md, "Rotating keys").
local function https_origin(url)
  return nodo.matches(url, "^https://[A-Za-z0-9.-]+(:[0-9]+)?$"), "must be an https:// origin with no path"
end

local function key_hex(key)
  return nodo.matches(key, "^[0-9a-f]{64}$"), "must be 64 lowercase hex characters"
end

return {
  api_version = 1,
  settings = {
    {
      name = "server_id",
      type = "url",
      label = "Public address",
      help = "The https origin this server is published at, such as https://pingers.example.org "
        .. "(server_id). It is the server's identity: RLN proofs and account registrations are "
        .. "bound to it, so changing it later makes a new server.",
      required = true,
      validate = https_origin,
    },
    {
      name = "operator",
      type = "string",
      label = "Operator",
      help = "Who runs this server, as /v1/params declares it (server_operator). It must be true "
        .. "and the same as the push relay's, so the app can warn that one operator runs both. "
        .. "By default it is the push relay's operator setting.",
      required = true,
      default = "$(pingers-push-relay.operator)",
      validate = function(name)
        return nodo.matches(name, "^\\S(.{0,126}\\S)?$"), "must be 1 to 128 characters without outer spaces"
      end,
    },
    {
      name = "ohttp_relay_url",
      type = "url",
      label = "OHTTP relay",
      help = "The relay URL /v1/params advertises (ohttp_relay_urls), such as "
        .. "https://ohttp.example.org/ohttp: the address pingers-ohttp-relay's site is "
        .. "published at, followed by /ohttp.",
      required = true,
      validate = function(url)
        return nodo.matches(url, "^https://[A-Za-z0-9.-]+(:[0-9]+)?/[A-Za-z0-9._~/-]*$"),
          "must be an https:// URL with a path"
      end,
    },
    {
      name = "map_host",
      type = "url",
      label = "Map host",
      help = "The map host /v1/params advertises (map_host), such as https://maps.example.org: "
        .. "the address pingers-map-host's site is published at.",
      required = true,
      validate = https_origin,
    },
    {
      name = "logging",
      type = "enum",
      label = "Logging",
      help = "off writes nothing. counts writes request totals once an hour, never an address "
        .. "or a single request (logging).",
      choices = { "off", "counts" },
      default = "off",
    },
    {
      name = "ohttp_key_a",
      type = "secret",
      label = "Gateway key A",
      help = "OHTTP gateway private key in slot A (ohttp.keys secret). Created on install. "
        .. "Reset only when slot A is not served and no supported app build pins it.",
      required = true,
      generate = { hex = 32 },
      validate = key_hex,
    },
    {
      name = "ohttp_key_b",
      type = "secret",
      label = "Gateway key B",
      help = "OHTTP gateway private key in slot B, published in advance so app builds can pin "
        .. "it before it is served (ohttp.keys secret). Created on install.",
      required = true,
      generate = { hex = 32 },
      validate = key_hex,
    },
    {
      name = "ohttp_key_a_id",
      type = "int",
      label = "Gateway key A id",
      help = "Key id of slot A (ohttp.keys key_id). Must differ from slot B's.",
      default = 1,
      min = 1,
      max = 255,
    },
    {
      name = "ohttp_key_b_id",
      type = "int",
      label = "Gateway key B id",
      help = "Key id of slot B (ohttp.keys key_id). Must differ from slot A's.",
      default = 2,
      min = 1,
      max = 255,
    },
    {
      -- TODO(p-server): the field name ohttp.served_key_id (P2).
      name = "ohttp_served_key_id",
      type = "int",
      label = "Served gateway key",
      help = "The key id GET /v1/ohttp/config serves (ohttp.served_key_id): slot A's or slot "
        .. "B's. Every configured key still opens requests, so changing it is the rotation.",
      default = 1,
      min = 1,
      max = 255,
    },
    {
      -- TODO(p-server): the field push_relays.public_url (P4), the push relay's address as
      -- /v1/params advertises it, now that url is the loopback address the server pushes to.
      name = "push_relay_public_url",
      type = "url",
      label = "Push relay address",
      help = "The push relay's public address, such as https://push.example.org: the address "
        .. "pingers-push-relay's site is published at. Devices fetch its key config there.",
      required = true,
      validate = https_origin,
    },
    {
      -- TODO(p-server, p-relays): push_relays.key_config_hash holds one hash today; with the
      -- relay's two-slot keys and `pingers-push-relay pins` this may become a list or move.
      name = "push_relay_key_config_hash",
      type = "string",
      label = "Push relay key hash",
      help = "SHA-256 of the push relay's served key config, as `pingers-push-relay pins` "
        .. "prints it (push_relays.key_config_hash). /v1/params lists it so devices can check "
        .. "the relay's key.",
      required = true,
      validate = function(hash)
        return nodo.matches(hash, "^[0-9a-f]{64}$"), "must be 64 lowercase hex characters"
      end,
    },
    {
      name = "gateway_url",
      type = "url",
      label = "Gateway (relay leg)",
      help = "Where pingers-ohttp-relay on this device forwards requests. Leave it as it is.",
      default = "http://127.0.0.1:9471/ohttp",
      public = "gateway-url",
      endpoint = "tcp",
    },
    {
      name = "gateway_key_config_url",
      type = "url",
      label = "Gateway key config (relay leg)",
      help = "Where pingers-ohttp-relay on this device fetches the served key config. Leave it "
        .. "as it is.",
      default = "http://127.0.0.1:9471/v1/ohttp/config",
      public = "gateway-key-config-url",
      endpoint = "tcp",
    },
  },
}
