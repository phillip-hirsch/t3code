-- tr3x: T3 Code threads as Rex tabs. `tr3x setup rex` prints how to load this
-- from init.lua. The tr3x daemon must be running (`tr3x daemon`).
--
--   require("tr3x")                                   -- `tr3x` is on Rex's PATH
--   require("tr3x").setup({ command = { "/path/to/bun", "/path/to/tr3x/src/cli.ts" } })

local M = { command = { "tr3x" } }

local TERMINAL = "com.superlogical.terminal"

function M.setup(options)
  if options ~= nil and options.command ~= nil then M.command = options.command end
  return M
end

local function argv(...)
  local result = {}
  for _, part in ipairs(M.command) do result[#result + 1] = part end
  for _, part in ipairs({ ... }) do result[#result + 1] = part end
  return result
end

-- Runs a tr3x command that prints one JSON line, and returns it decoded.
local function tr3x(...)
  local result, err = rex.exec(argv(...), { timeout = 25 })
  if result == nil then error("tr3x: " .. tostring(err), 0) end
  if not result.ok then
    local message = result.stderr ~= "" and result.stderr or ("tr3x exited with " .. result.code)
    error(message, 0)
  end
  return rex.json.decode(result.stdout)
end

-- Shows the tab's session in the app that ran the action, from a key or the
-- palette. `rex do` on the CLI has no app to ask.
local function show(ctx, tab)
  local from_app = ctx.origin == "key" or ctx.origin == "palette"
  if tab ~= nil and from_app and ctx.client_id ~= nil and tab.sessionId ~= ctx.session_id then
    pcall(rex.client.queue, "session.select", { session_id = tab.sessionId })
  end
  return tab
end

-- Where the focused terminal is: its foreground process's directory.
local function focused_cwd(ctx)
  if ctx.block_id == nil then return nil end
  local process = rex.block.call(TERMINAL, "process", { block_id = ctx.block_id })
  if process ~= nil and process.foreground ~= nil then return process.foreground.cwd end
  return nil
end

rex.action({
  name = "tr3x_new_thread",
  title = "New Thread",
  category = "tr3x",
  description = "Start a T3 thread in the focused terminal's directory",
  keywords = "t3 agent codex claude",
  run = function(ctx)
    local cwd = focused_cwd(ctx) or os.getenv("HOME")
    return show(ctx, tr3x("new", "--cwd", cwd))
  end,
})

rex.action({
  name = "tr3x_open_thread",
  title = "Open Thread…",
  category = "tr3x",
  description = "Choose a T3 thread to open in a tab",
  keywords = "t3 agent pick switch",
  run = function(ctx)
    local layer, err = rex.session.new_layer({
      bounds = { 0.15, 0.1, 0.7, 0.6 },
      focus = true,
      layout = rex.layout.block({
        flavor = TERMINAL .. ".shell",
        -- The app that asked, so the picker can show it the session the thread opens in.
        options = { command = ctx.client_id and argv("pick", "--client", ctx.client_id) or argv("pick") },
      }),
    })
    if layer == nil then error(err, 0) end
  end,
})

rex.action({
  name = "tr3x_next_blocked",
  title = "Next Blocked Thread",
  category = "tr3x",
  description = "Go to the next T3 thread waiting on you",
  keywords = "t3 agent approval question waiting",
  run = function(ctx)
    local tab = tr3x("next-blocked")
    if tab == nil then
      rex.log("tr3x: no thread is waiting on you")
      return nil
    end
    return show(ctx, tab)
  end,
})

rex.bind("cmd+shift+t", "tr3x_new_thread")
rex.bind("cmd+shift+o", "tr3x_open_thread")
rex.bind("cmd+shift+b", "tr3x_next_blocked")

return M
