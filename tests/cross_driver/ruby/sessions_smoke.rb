# Cross-driver logical-sessions smoke — Ruby (mongo-ruby-driver).
#
# Drives startSession / endSessions / refreshSessions through raw
# runCommand so the wire-level lsid round-trip is what's exercised
# (not the driver's high-level Session API).
require "mongo"

def fail_with(msg)
  warn(msg)
  exit(1)
end

uri = ENV["MONGODB_URI"] or fail_with("MONGODB_URI not set")
Mongo::Logger.logger.level = Logger::FATAL

client = Mongo::Client.new(uri, database: "admin", server_selection_timeout: 5)
begin
  # 1. startSession returns {id: BinData(4, uuid), timeoutMinutes}.
  started = client.database.command(startSession: 1).first
  fail_with("startSession failed: #{started.inspect}") unless started["ok"] == 1
  wrapped = started["id"]
  fail_with("startSession id wrapper missing: #{started.inspect}") unless wrapped.is_a?(Hash) || wrapped.respond_to?(:to_h)
  inner = (wrapped.respond_to?(:to_h) ? wrapped.to_h : wrapped)["id"]
  fail_with("lsid not BSON::Binary: #{inner.inspect}") unless inner.is_a?(BSON::Binary)
  fail_with("lsid subtype: #{inner.type}") unless inner.type == :uuid
  fail_with("lsid length: #{inner.data.bytesize}") unless inner.data.bytesize == 16
  fail_with("timeoutMinutes: #{started['timeoutMinutes']}") unless started["timeoutMinutes"] == 30

  # 2. endSessions on the new lsid → ok.
  ended = client.database.command(endSessions: [{"id" => inner}]).first
  fail_with("endSessions: #{ended.inspect}") unless ended["ok"] == 1

  # 3. refreshSessions implicit-creates an unknown lsid; server accepts.
  fake = BSON::Binary.new("0123456789abcdef", :uuid)
  refreshed = client.database.command(refreshSessions: [{"id" => fake}]).first
  fail_with("refreshSessions: #{refreshed.inspect}") unless refreshed["ok"] == 1

  puts "OK"
ensure
  client.close
end
