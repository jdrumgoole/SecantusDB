package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import com.mongodb.client.MongoCollection;
import org.bson.BsonBinarySubType;
import org.bson.Document;
import org.bson.types.Binary;
import org.bson.types.Decimal128;
import org.bson.types.ObjectId;

import java.time.Instant;
import java.util.Date;
import java.util.List;

/**
 * Cross-driver BSON type fidelity smoke — Java.
 * <p>
 * Insert one document containing every BSON type the Java driver
 * surfaces as a distinct class, then find it back and assert each
 * field preserved its class and value. Java's BSON codec is a separate
 * implementation from pymongo's, so any wire-shape divergence trips
 * here with a class-mismatch.
 */
public final class TypesSmoke {

    private TypesSmoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        if (uri == null) {
            System.err.println("MONGODB_URI not set");
            System.exit(2);
        }

        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .applyToClusterSettings(b -> b.serverSelectionTimeout(30,
                java.util.concurrent.TimeUnit.SECONDS))
            .build();

        try (MongoClient client = MongoClients.create(settings)) {
            MongoCollection<Document> coll = client.getDatabase("types_xd").getCollection("c");
            coll.drop();

            ObjectId objID = new ObjectId();
            Decimal128 dec = Decimal128.parse("3.141592653589793238");
            Date when = Date.from(Instant.parse("2026-05-06T12:34:56.789Z"));
            Binary bin = new Binary(BsonBinarySubType.BINARY, "hello".getBytes());

            // Java's int / long / double are distinct primitive types; BSON
            // encodes int as int32, long as int64, double as double — no
            // need for explicit wrapper classes the way Node does. Mixing
            // them in one doc exercises the same int32/int64 fidelity path.
            Document doc = new Document()
                .append("_id", objID)
                .append("i32", 2_147_483_647)
                .append("i64", 9_223_372_036_854_775_807L)
                .append("f64", 2.5)
                .append("dec", dec)
                .append("dt", when)
                .append("bin", bin)
                .append("b", true)
                .append("n", null)
                .append("sub", new Document("x", 1))
                .append("arr", List.of(1, "two", 3.5));

            coll.insertOne(doc);

            Document got = coll.find(new Document("_id", objID)).first();
            if (got == null) {
                System.err.println("findOne returned null");
                System.exit(1);
            }

            if (!objID.equals(got.get("_id"))) {
                System.err.printf("_id: got %s (%s)%n", got.get("_id"),
                    got.get("_id").getClass().getName());
                System.exit(1);
            }
            if (!(got.get("i32") instanceof Integer) || got.getInteger("i32") != 2_147_483_647) {
                System.err.printf("i32: got %s (%s)%n", got.get("i32"),
                    got.get("i32").getClass().getName());
                System.exit(1);
            }
            if (!(got.get("i64") instanceof Long) || got.getLong("i64") != 9_223_372_036_854_775_807L) {
                System.err.printf("i64: got %s (%s)%n", got.get("i64"),
                    got.get("i64").getClass().getName());
                System.exit(1);
            }
            if (!(got.get("f64") instanceof Double) || got.getDouble("f64") != 2.5) {
                System.err.printf("f64: got %s (%s)%n", got.get("f64"),
                    got.get("f64").getClass().getName());
                System.exit(1);
            }
            if (!(got.get("dec") instanceof Decimal128) || !dec.equals(got.get("dec"))) {
                System.err.printf("dec: got %s (%s)%n", got.get("dec"),
                    got.get("dec").getClass().getName());
                System.exit(1);
            }
            if (!(got.get("dt") instanceof Date) || ((Date) got.get("dt")).getTime() != when.getTime()) {
                System.err.printf("dt: got %s%n", got.get("dt"));
                System.exit(1);
            }
            Object binGot = got.get("bin");
            if (!(binGot instanceof Binary)) {
                System.err.printf("bin: got %s (%s)%n", binGot,
                    binGot == null ? "null" : binGot.getClass().getName());
                System.exit(1);
            }
            Binary binTyped = (Binary) binGot;
            if (binTyped.getType() != 0 || !"hello".equals(new String(binTyped.getData()))) {
                System.err.printf("bin: subType=%d data=%s%n", binTyped.getType(),
                    new String(binTyped.getData()));
                System.exit(1);
            }
            if (!Boolean.TRUE.equals(got.get("b"))) {
                System.err.printf("b: got %s%n", got.get("b"));
                System.exit(1);
            }
            if (got.get("n") != null) {
                System.err.printf("n: got %s%n", got.get("n"));
                System.exit(1);
            }
            Document sub = got.get("sub", Document.class);
            if (sub == null || !(sub.get("x") instanceof Integer) || sub.getInteger("x") != 1) {
                System.err.printf("sub.x: got %s%n", sub == null ? null : sub.get("x"));
                System.exit(1);
            }
            @SuppressWarnings("unchecked")
            List<Object> arr = (List<Object>) got.get("arr");
            if (arr == null || arr.size() != 3) {
                System.err.printf("arr: got %s%n", arr);
                System.exit(1);
            }
            if (!(arr.get(0) instanceof Integer) || ((Integer) arr.get(0)) != 1) {
                System.err.printf("arr[0]: got %s%n", arr.get(0));
                System.exit(1);
            }
            if (!"two".equals(arr.get(1))) {
                System.err.printf("arr[1]: got %s%n", arr.get(1));
                System.exit(1);
            }
            if (!(arr.get(2) instanceof Double) || ((Double) arr.get(2)) != 3.5) {
                System.err.printf("arr[2]: got %s%n", arr.get(2));
                System.exit(1);
            }

            System.out.println("OK");
        }
    }
}
