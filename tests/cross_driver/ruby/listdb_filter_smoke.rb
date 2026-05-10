# Cross-driver listDatabases filter smoke — Ruby (mongo-ruby-driver).
#
# Insert one doc into three databases, then list with `name: alpha`
# filter and assert only that one is returned. Re-list with
# `name_only: true` and assert at least three dbs are reported.
require "mongo"

def fail_with(msg)
  warn(msg)
  exit(1)
end

uri = ENV["MONGODB_URI"] or fail_with("MONGODB_URI not set")
Mongo::Logger.logger.level = Logger::FATAL

client = Mongo::Client.new(uri, server_selection_timeout: 5)
begin
  ["alpha", "beta", "gamma"].each do |db_name|
    client.use(db_name)["c"].insert_one("_id" => 1)
  end

  begin
    filtered = client.use("admin").database.command(
      listDatabases: 1,
      filter: {"name" => "alpha"},
    ).first
    names = (filtered["databases"] || []).map { |d| d["name"] }
    unless names == ["alpha"]
      fail_with("filter: got #{names.inspect}, want [alpha]")
    end

    name_only = client.use("admin").database.command(
      listDatabases: 1,
      nameOnly: true,
    ).first
    dbs = name_only["databases"] || []
    if dbs.length < 3
      fail_with("nameOnly: got #{dbs.length} dbs, want >= 3")
    end

    puts "OK"
  ensure
    ["alpha", "beta", "gamma"].each do |db_name|
      begin
        client.use(db_name).database.drop
      rescue StandardError
        # best-effort cleanup
      end
    end
  end
ensure
  client.close
end
