# Cross-driver RBAC smoke — Ruby (mongo-ruby-driver).
#
# Provisions a ``read``-bound user via the root admin connection,
# then asserts find works and insert is rejected with code 13 /
# Unauthorized when authenticated as the new user.
require "mongo"

def fail_with(msg)
  warn(msg)
  exit(1)
end

uri = ENV["MONGODB_URI"] or fail_with("MONGODB_URI not set")
admin_pwd = ENV["ADMIN_PASSWORD"] or fail_with("ADMIN_PASSWORD not set")
Mongo::Logger.logger.level = Logger::FATAL

root = Mongo::Client.new(
  uri,
  database: "admin",
  user: "root",
  password: admin_pwd,
  auth_source: "admin",
  auth_mech: :scram256,
  server_selection_timeout: 5,
)
begin
  root.use("shop").database.command(
    createUser: "viewer_ruby",
    pwd: "vp",
    roles: [{role: "read", db: "shop"}],
  )
ensure
  root.close
end

viewer = Mongo::Client.new(
  uri,
  database: "shop",
  user: "viewer_ruby",
  password: "vp",
  auth_source: "shop",
  auth_mech: :scram256,
  server_selection_timeout: 5,
)
begin
  # Read should succeed.
  viewer["items"].find({}).to_a

  caught = nil
  begin
    viewer["items"].insert_one({"x" => 1})
  rescue Mongo::Error::OperationFailure => e
    caught = e
  end
  fail_with("insert should have been rejected") unless caught
  unless caught.code == 13 || caught.message.include?("Unauthorized")
    fail_with("unexpected error: code=#{caught.code} message=#{caught.message}")
  end

  puts "OK"
ensure
  viewer.close
end
