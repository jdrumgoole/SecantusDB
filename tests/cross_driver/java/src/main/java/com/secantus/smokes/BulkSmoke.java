package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.bulk.BulkWriteResult;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import com.mongodb.client.MongoCollection;
import com.mongodb.client.model.DeleteOneModel;
import com.mongodb.client.model.InsertOneModel;
import com.mongodb.client.model.ReplaceOneModel;
import com.mongodb.client.model.ReplaceOptions;
import com.mongodb.client.model.UpdateManyModel;
import com.mongodb.client.model.UpdateOneModel;
import com.mongodb.client.model.UpdateOptions;
import com.mongodb.client.model.WriteModel;
import org.bson.Document;
import org.bson.conversions.Bson;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;

/**
 * Cross-driver bulk-write smoke — Java.
 * <p>
 * One mixed bulkWrite that insert / update / replace / upsert /
 * delete-walks across six docs, then asserts the result counts and
 * final collection state. The Java driver's bulk command builder is
 * a separate implementation from pymongo's.
 */
public final class BulkSmoke {

    private BulkSmoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        if (uri == null) {
            System.err.println("MONGODB_URI not set");
            System.exit(2);
        }

        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .applyToClusterSettings(b -> b.serverSelectionTimeout(30, TimeUnit.SECONDS))
            .build();

        try (MongoClient client = MongoClients.create(settings)) {
            MongoCollection<Document> coll = client.getDatabase("bulk_xd").getCollection("c");
            coll.drop();

            coll.insertMany(List.of(
                new Document("_id", 1).append("kind", "old"),
                new Document("_id", 2).append("kind", "old")));

            List<WriteModel<Document>> ops = new ArrayList<>();
            ops.add(new InsertOneModel<>(new Document("_id", 3).append("kind", "fresh")));
            ops.add(new UpdateOneModel<>(
                new Document("_id", 1),
                new Document("$set", new Document("kind", "new"))));
            ops.add(new UpdateManyModel<>(
                new Document("kind", "old"),
                new Document("$set", new Document("kind", "new"))));
            ops.add(new ReplaceOneModel<>(
                new Document("_id", 3),
                new Document("_id", 3).append("kind", "replaced"),
                new ReplaceOptions()));
            ops.add(new UpdateOneModel<>(
                new Document("_id", 99),
                new Document("$set", new Document("kind", "upserted")),
                new UpdateOptions().upsert(true)));
            ops.add(new DeleteOneModel<>(new Document("_id", 2)));

            BulkWriteResult res = coll.bulkWrite(ops);

            if (res.getInsertedCount() != 1) {
                System.err.printf("insertedCount: got %d, want 1%n", res.getInsertedCount());
                System.exit(1);
            }
            if (res.getMatchedCount() != 3) {
                System.err.printf("matchedCount: got %d, want 3%n", res.getMatchedCount());
                System.exit(1);
            }
            if (res.getModifiedCount() != 3) {
                System.err.printf("modifiedCount: got %d, want 3%n", res.getModifiedCount());
                System.exit(1);
            }
            if (res.getUpserts().size() != 1) {
                System.err.printf("upsertedCount: got %d, want 1%n", res.getUpserts().size());
                System.exit(1);
            }
            if (res.getDeletedCount() != 1) {
                System.err.printf("deletedCount: got %d, want 1%n", res.getDeletedCount());
                System.exit(1);
            }

            List<Document> got = new ArrayList<>();
            Bson sort = new Document("_id", 1);
            coll.find(new Document()).sort(sort).into(got);
            if (got.size() != 3) {
                System.err.printf("final docs: got %d, want 3%n", got.size());
                System.exit(1);
            }
            if (got.get(0).getInteger("_id") != 1 || !"new".equals(got.get(0).getString("kind"))) {
                System.err.printf("doc[0]: got %s%n", got.get(0).toJson());
                System.exit(1);
            }
            if (got.get(1).getInteger("_id") != 3 || !"replaced".equals(got.get(1).getString("kind"))) {
                System.err.printf("doc[1]: got %s%n", got.get(1).toJson());
                System.exit(1);
            }
            if (got.get(2).getInteger("_id") != 99 || !"upserted".equals(got.get(2).getString("kind"))) {
                System.err.printf("doc[2]: got %s%n", got.get(2).toJson());
                System.exit(1);
            }
            System.out.println("OK");
        }
    }
}
