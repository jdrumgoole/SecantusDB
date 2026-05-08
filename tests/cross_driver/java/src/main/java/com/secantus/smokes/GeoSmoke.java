package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import com.mongodb.client.MongoCollection;
import com.mongodb.client.model.Indexes;
import org.bson.Document;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.TimeUnit;

/**
 * Cross-driver geo smoke — Java.
 * <p>
 * Insert three GeoJSON Points, build a {@code 2dsphere} index, and
 * exercise {@code $geoWithin} (set match) plus a {@code $geoNear}
 * aggregation pipeline with maxDistance in metres. Distance bounds
 * mirror the node/go versions: doc 0 is at the query point, doc 1 is
 * ~111 m away, doc 3 is well outside the 200 m max.
 */
public final class GeoSmoke {

    private GeoSmoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        if (uri == null) {
            System.err.println("MONGODB_URI not set");
            System.exit(2);
        }

        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .applyToClusterSettings(b -> b.serverSelectionTimeout(5, TimeUnit.SECONDS))
            .build();

        try (MongoClient client = MongoClients.create(settings)) {
            MongoCollection<Document> coll = client.getDatabase("geo_xdriver")
                .getCollection("places");
            coll.drop();

            coll.insertMany(List.of(
                new Document("_id", 1).append("loc", new Document()
                    .append("type", "Point")
                    .append("coordinates", List.of(0.0, 0.0))),
                new Document("_id", 2).append("loc", new Document()
                    .append("type", "Point")
                    .append("coordinates", List.of(0.001, 0.0))),
                new Document("_id", 3).append("loc", new Document()
                    .append("type", "Point")
                    .append("coordinates", List.of(50.0, 50.0)))));
            coll.createIndex(Indexes.geo2dsphere("loc"));

            // $geoWithin — set comparison since order is unspecified.
            List<Document> within = new ArrayList<>();
            coll.find(new Document("loc", new Document("$geoWithin",
                new Document("$centerSphere", List.of(List.of(0, 0), 0.001)))))
                .into(within);
            Set<Integer> ids = new HashSet<>();
            for (Document d : within) {
                ids.add(d.getInteger("_id"));
            }
            if (!ids.equals(Set.of(1, 2))) {
                System.err.printf("$geoWithin: got %s, want [1, 2]%n", ids);
                System.exit(1);
            }

            // $geoNear — ordered by ascending distance, maxDistance in metres.
            List<Document> agg = new ArrayList<>();
            coll.aggregate(List.of(new Document("$geoNear", new Document()
                .append("near", new Document()
                    .append("type", "Point")
                    .append("coordinates", List.of(0, 0)))
                .append("distanceField", "d")
                .append("key", "loc")
                .append("maxDistance", 200))))
                .into(agg);

            List<Integer> aggIds = new ArrayList<>();
            for (Document d : agg) {
                aggIds.add(d.getInteger("_id"));
            }
            if (!aggIds.equals(List.of(1, 2))) {
                System.err.printf("$geoNear order: got %s, want [1, 2]%n", aggIds);
                System.exit(1);
            }

            double d0 = agg.get(0).getDouble("d");
            double d1 = agg.get(1).getDouble("d");
            if (d0 > 0.001) {
                System.err.printf("$geoNear d[0]: got %f, want ~0%n", d0);
                System.exit(1);
            }
            if (d1 < 100 || d1 > 130) {
                System.err.printf("$geoNear d[1]: got %f, want ~111%n", d1);
                System.exit(1);
            }
            System.out.println("OK");
        }
    }
}
