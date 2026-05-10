# Cross-driver cluster-role-bundle smoke — Ruby.
#
# Provisions a clusterMonitor user and a backup user, verifies
# clusterMonitor can listDatabases but not insert, and that backup
# can read every db but not insert. Both rejection paths return
# code 13 / Unauthorized.
require "mongo"

def fail_with(msg)
  warn(msg)
  exit(1)
end

uri = ENV["MONGODB_URI"] or fail_with("MONGODB_URI not set")
admin_pwd = ENV["ADMIN_PASSWORD"] or fail_with("ADMIN_PASSWORD not set")
Mongo::Logger.logger.level = Logger::FATAL

def authed_client(uri, user, pwd, auth_source)
  Mongo::Client.new(
    uri,
    database: auth_source,
    user: user,
    password: pwd,
    auth_source: auth_source,
    auth_mech: :scram256,
    server_selection_timeout: 5,
  )
end

root = authed_client(uri, "root", admin_pwd, "admin")
begin
  root.use("admin").database.command(
    createUser: "cluster_mon_ruby",
    pwd: "p",
    roles: [{role: "clusterMonitor", db: "admin"}],
  )
  root.use("admin").database.command(
    createUser: "backup_user_ruby",
    pwd: "p",
    roles: [{role: "backup", db: "admin"}],
  )
  root.use("shop")["items"].insert_one("_id" => 1, "name" => "thing")
ensure
  root.close
end

# 1. clusterMonitor: listDatabases ok, insert rejected.
cm = authed_client(uri, "cluster_mon_ruby", "p", "admin")
begin
  ldb = cm.use("admin").database.command(listDatabases: 1).first
  unless ldb["ok"] == 1 && (ldb["databases"] || []).is_a?(Array)
    fail_with("clusterMonitor listDatabases: #{ldb.inspect}")
  end

  caught = nil
  begin
    cm.use("shop")["items"].insert_one("_id" => 99, "x" => 1)
  rescue Mongo::Error::OperationFailure => e
    caught = e
  end
  fail_with("clusterMonitor insert should have been rejected") unless caught
  unless caught.code == 13 || caught.message.include?("Unauthorized")
    fail_with("clusterMonitor insert code=#{caught.code} message=#{caught.message}")
  end
ensure
  cm.close
end

# 2. backup: read on every db ok, insert rejected.
bk = authed_client(uri, "backup_user_ruby", "p", "admin")
begin
  docs = bk.use("shop")["items"].find({}).to_a
  unless docs.length == 1 && docs.first["_id"] == 1
    fail_with("backup read: got #{docs.inspect}")
  end

  caught = nil
  begin
    bk.use("shop")["items"].insert_one("_id" => 99, "x" => 1)
  rescue Mongo::Error::OperationFailure => e
    caught = e
  end
  fail_with("backup insert should have been rejected") unless caught
  unless caught.code == 13 || caught.message.include?("Unauthorized")
    fail_with("backup insert code=#{caught.code} message=#{caught.message}")
  end

  puts "OK"
ensure
  bk.close
end
