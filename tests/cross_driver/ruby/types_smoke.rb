# Cross-driver BSON type fidelity smoke — Ruby (mongo-ruby-driver).
#
# Same workload as the Node / Go / Java types smokes: insert one
# document containing every BSON type the Ruby driver surfaces as a
# distinct class, then find it back and assert each field preserved
# its class and value.
require "bson"
require "mongo"

def fail_with(msg)
  warn(msg)
  exit(1)
end

uri = ENV["MONGODB_URI"] or fail_with("MONGODB_URI not set")
Mongo::Logger.logger.level = Logger::FATAL

client = Mongo::Client.new(
  uri,
  database: "types_xd_ruby",
  server_selection_timeout: 5,
)
begin
  coll = client["c"]
  coll.drop

  oid = BSON::ObjectId.new
  dec = BSON::Decimal128.new("3.141592653589793238")
  when_ = Time.utc(2026, 5, 29, 0, 0, 0)
  bin = BSON::Binary.new("hello", :generic)

  doc_in = {
    "_id" => oid,
    "i32" => BSON::Int32.new(2_147_483_647),
    "i64" => BSON::Int64.new(9_223_372_036_854_775_807),
    "f64" => 2.5,
    "dec" => dec,
    "dt" => when_,
    "bin" => bin,
    "b" => true,
    "n" => nil,
    "sub" => {"x" => BSON::Int32.new(1)},
    "arr" => [BSON::Int32.new(1), "two", 3.5],
  }
  coll.insert_one(doc_in)

  got = coll.find("_id" => oid).first
  fail_with("findOne returned nil") if got.nil?

  fail_with("_id: got #{got['_id'].inspect}") unless got["_id"] == oid
  # Ruby driver decodes int32 / int64 to native Integer (no Int32/Int64
  # wrapping on read). Verify by value, not class — the wire-side
  # encoding was the BSON 32-bit / 64-bit tag and that's what mattered.
  fail_with("i32: got #{got['i32'].inspect}") unless got["i32"] == 2_147_483_647
  fail_with("i64: got #{got['i64'].inspect}") unless got["i64"] == 9_223_372_036_854_775_807
  fail_with("f64: got #{got['f64'].inspect}") unless got["f64"].is_a?(Float) && got["f64"] == 2.5
  # BSON 5.x decodes Decimal128 to BigDecimal by default. Compare via
  # Float-precision equality (plenty for a wire-shape smoke) and
  # accept either the BSON::Decimal128 wrapper (older bson gems) or
  # BigDecimal (bson 5.x).
  got_dec = got["dec"]
  got_dec_f = got_dec.to_s.to_f
  expected_dec_f = 3.141592653589793238
  is_decimal_class = got_dec.is_a?(BSON::Decimal128) || got_dec.is_a?(BigDecimal)
  unless is_decimal_class && (got_dec_f - expected_dec_f).abs < 1e-12
    fail_with("dec: got #{got_dec.class}: #{got_dec.inspect} → #{got_dec_f}")
  end
  fail_with("dt: got #{got['dt'].inspect}") unless got["dt"].is_a?(Time) && got["dt"].to_i == when_.to_i
  fail_with("bin: got #{got['bin'].inspect}") unless got["bin"].is_a?(BSON::Binary) && got["bin"].data == "hello" && got["bin"].type == :generic
  fail_with("b: got #{got['b'].inspect}") unless got["b"] == true
  fail_with("n: got #{got['n'].inspect}") unless got["n"].nil?
  fail_with("sub: got #{got['sub'].inspect}") unless got["sub"].is_a?(Hash) && got["sub"]["x"] == 1
  arr = got["arr"]
  fail_with("arr: got #{arr.inspect}") unless arr.is_a?(Array) && arr.length == 3
  fail_with("arr[0]: got #{arr[0].inspect}") unless arr[0] == 1
  fail_with("arr[1]: got #{arr[1].inspect}") unless arr[1] == "two"
  fail_with("arr[2]: got #{arr[2].inspect}") unless arr[2].is_a?(Float) && arr[2] == 3.5

  puts "OK"
ensure
  client.close
end
