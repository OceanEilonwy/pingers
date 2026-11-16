-- pingers's settings (setup.toml.in). Everything the setup page shows comes from the four
-- services; these only say where the app builds' variables go and which tag to suggest.
return {
  api_version = 1,
  settings = {
    {
      name = "registration_code",
      type = "secret",
      label = "Registration code",
      help = "pingers-server's registration code, read through the grant approved with the "
        .. "install, for the setup page and the first phone's link. Reset it in pingers-server.",
      required = true,
      default = "$(pingers-server.registration_code)",
    },
    {
      name = "github_repository",
      type = "string",
      label = "GitHub repository",
      help = "The repository whose release builds carry this appliance's addresses and pins "
        .. "(owner/name). The setup page names its variables.",
      required = true,
      default = "OceanEilonwy/pingers",
      validate = function(repository)
        return nodo.matches(repository, "^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$"),
          "must be owner/name"
      end,
    },
    {
      name = "release_tag",
      type = "string",
      label = "Release tag",
      help = "The tag the setup page suggests for the next release, such as v1.0.0-rc.1.",
      required = true,
      default = "v1.0.0-rc.1",
      validate = function(tag)
        return nodo.matches(tag, "^v[0-9]+\\.[0-9]+\\.[0-9]+(-(alpha|beta|rc)\\.[0-9]+)?$"),
          "must be a release tag such as v1.0.0 or v1.0.0-rc.1"
      end,
    },
  },
}
