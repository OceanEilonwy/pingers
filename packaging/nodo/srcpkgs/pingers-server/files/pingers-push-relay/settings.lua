-- pingers-push-relay's settings. Each help text names the push-relay.toml field it fills
-- (push-relay.toml.in).
--
-- The hash key, both HPKE keys and the server credential generate themselves on install. The
-- FCM service account is the one thing the owner pastes, on the device, in nodo-tui: no tool
-- ever reads or copies it. Nodo settings hold one line, so paste the JSON key file on one line
-- (its private key keeps its \n escapes).
local function key_hex(key)
  return nodo.matches(key, "^[0-9a-f]{64}$"), "must be 64 lowercase hex characters"
end

return {
  api_version = 1,
  settings = {
    {
      -- Asked once when the pingers bundle is installed. It lives here because the push relay
      -- is the root of the references: pingers-server and pingers derive their addresses from
      -- it without a cycle.
      name = "domain",
      type = "string",
      label = "Your domain",
      help = "The domain the four Pingers sites are named under, such as example.org: "
        .. "pingers., ohttp., push. and maps. pingers-server's addresses default to those "
        .. "names, and can each be changed there.",
      required = true,
      public = "domain",
      validate = function(domain)
        return nodo.matches(domain, "^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\\.)+[a-z]{2,63}$"),
          "must be a domain name in lower case, such as example.org"
      end,
    },
    {
      name = "operator",
      type = "string",
      label = "Operator",
      help = "Who runs this relay, as GET /v1/config declares it (operator). It must be true. "
        .. "By default it is your domain. pingers-server uses it as its own operator by "
        .. "default, so both say the same.",
      required = true,
      default = "$(pingers-push-relay.domain)",
      public = "operator",
      validate = function(name)
        return nodo.matches(name, "^\\S(.{0,126}\\S)?$"), "must be 1 to 128 characters without outer spaces"
      end,
    },
    {
      name = "logging",
      type = "enum",
      label = "Logging",
      help = "off writes nothing. counts writes outcome totals once an hour, never a token, "
        .. "an address or a single push (logging).",
      choices = { "off", "counts" },
      default = "off",
    },
    {
      name = "hash_key",
      type = "secret",
      label = "Hash key",
      help = "Keys the server credential's hash and the per-token rate limits (hash_key). "
        .. "Created on install.",
      required = true,
      generate = { hex = 32 },
      validate = key_hex,
    },
    {
      name = "key_a",
      type = "secret",
      label = "Relay key A",
      help = "HPKE private key in slot A (keys private_key). Created on install. Reset only when "
        .. "slot A is retired and no supported app build pins it.",
      required = true,
      generate = { hex = 32 },
      validate = key_hex,
    },
    {
      name = "key_b",
      type = "secret",
      label = "Relay key B",
      help = "HPKE private key in slot B, published before it activates so app builds can pin "
        .. "it (keys private_key). Created on install.",
      required = true,
      generate = { hex = 32 },
      validate = key_hex,
    },
    {
      name = "key_a_id",
      type = "int",
      label = "Relay key A id",
      help = "Key id of slot A (keys key_id). Must differ from slot B's.",
      default = 1,
      min = 1,
      max = 255,
    },
    {
      name = "key_b_id",
      type = "int",
      label = "Relay key B id",
      help = "Key id of slot B (keys key_id). Must differ from slot A's.",
      default = 2,
      min = 1,
      max = 255,
    },
    {
      name = "key_a_activated_at",
      type = "int",
      label = "Key A activates",
      help = "Unix seconds when slot A starts sealing (keys activated_at). 0: from the start.",
      default = 0,
      min = 0,
    },
    {
      name = "key_b_activated_at",
      type = "int",
      label = "Key B activates",
      help = "Unix seconds when slot B starts sealing (keys activated_at). The default, "
        .. "4102444800 (2100-01-01), keeps it in reserve until a rotation sets it to now.",
      default = 4102444800,
      min = 0,
    },
    {
      name = "key_a_retired_at",
      type = "int",
      label = "Key A retires",
      help = "Unix seconds after which slot A no longer opens anything (keys retired_at), at "
        .. "least 30 days after slot B activates. 0: not scheduled.",
      default = 0,
      min = 0,
    },
    {
      name = "key_b_retired_at",
      type = "int",
      label = "Key B retires",
      help = "Unix seconds after which slot B no longer opens anything (keys retired_at). "
        .. "0: not scheduled.",
      default = 0,
      min = 0,
    },
    {
      name = "server_credential",
      type = "secret",
      label = "Server credential",
      help = "The bearer credential pingers-server presents (servers credential). Created on "
        .. "install; pingers-server reads it once the owner approves its grant. Reset changes "
        .. "both packages together.",
      required = true,
      generate = { hex = 32 },
      validate = key_hex,
    },
    {
      name = "fcm_service_account",
      type = "secret",
      label = "Firebase service account",
      help = "The JSON key of a service account with only the Firebase Cloud Messaging API "
        .. "Admin role (fcm service_account_json); the relay takes the Firebase project from its "
        .. "project_id. Paste it here yourself; to rotate, add a new key in the Google Cloud "
        .. "console, paste it, then delete the old one.",
      required = true,
      validate = function(json)
        return nodo.matches(json, "^[ ]*\\{.*\"private_key\".*\\}[ ]*$"),
          "must be the service account's JSON key file on one line"
      end,
    },
    {
      name = "server_url",
      type = "url",
      label = "Push address (server leg)",
      help = "Where pingers-server on this device sends pushes, on loopback. Leave it as it is.",
      default = "http://127.0.0.1:9472",
      public = "server-url",
      endpoint = "tcp",
    },
  },
  -- The Firebase project now comes from the pasted key file's project_id.
  retired = { "fcm_project_id" },
}
