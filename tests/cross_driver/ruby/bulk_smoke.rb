# Cross-driver bulk-write smoke — Ruby (mongo-ruby-driver).
#
# One mixed bulk_write that insert / update / replace / upsert /
# delete-walks across six docs, then asserts the result counts and
# final collection state. The Ruby driver's bulk command builder is
# a separate implementation from pymongo's.
require "mongo"

def fail_with(msg)
  warn(msg)
  exit(1)
end

uri = ENV["MONGODB_URI"] or fail_with("MONGODB_URI not set")
Mongo::Logger.logger.level = Logger::FATAL

client = Mongo::Client.new(uri, database: "bulk_xd_ruby", server_selection_timeout: 5)
begin
  coll = client["c"]
  coll.drop

  coll.insert_many([
    {"_id" => 1, "kind" => "old"},
    {"_id" => 2, "kind" => "old"},
  ])

  res = coll.bulk_write([
    {insert_one: {"_id" => 3, "kind" => "fresh"}},
    {update_one: {filter: {"_id" => 1}, update: {"$set" => {"kind" => "new"}}}},
    {update_many: {filter: {"kind" => "old"}, update: {"$set" => {"kind" => "new"}}}},
    {replace_one: {filter: {"_id" => 3}, replacement: {"_id" => 3, "kind" => "replaced"}}},
    {update_one: {filter: {"_id" => 99}, update: {"$set" => {"kind" => "upserted"}}, upsert: true}},
    {delete_one: {filter: {"_id" => 2}}},
  ])

  fail_with("inserted_count: got #{res.inserted_count}, want 1") unless res.inserted_count == 1
  fail_with("matched_count: got #{res.matched_count}, want 3") unless res.matched_count == 3
  fail_with("modified_count: got #{res.modified_count}, want 3") unless res.modified_count == 3
  fail_with("upserted_count: got #{res.upserted_count}, want 1") unless res.upserted_count == 1
  fail_with("deleted_count: got #{res.deleted_count}, want 1") unless res.deleted_count == 1

  got = coll.find({}).sort(_id: 1).to_a
  fail_with("final docs: got #{got.length}, want 3") unless got.length == 3
  fail_with("doc[0]: #{got[0]}") unless got[0]["_id"] == 1 && got[0]["kind"] == "new"
  fail_with("doc[1]: #{got[1]}") unless got[1]["_id"] == 3 && got[1]["kind"] == "replaced"
  fail_with("doc[2]: #{got[2]}") unless got[2]["_id"] == 99 && got[2]["kind"] == "upserted"

  puts "OK"
ensure
  client.close
end
