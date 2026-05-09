package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.MongoCommandException;
import com.mongodb.MongoCredential;
import com.mongodb.MongoWriteException;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import org.bson.Document;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;

/**
 * Cross-driver cluster-role-bundle smoke — Java.
 * <p>
 * Provisions a {@code clusterMonitor} user and a {@code backup} user
 * (both admin-bound), then asserts:
 *  - clusterMonitor can {@code listDatabases} but cannot {@code insert}
 *  - backup can {@code find} on any db but cannot {@code insert}
 * Both rejection paths return code 13 / Unauthorized.
 */
public final class ClusterRolesSmoke {

    private ClusterRolesSmoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        String adminPwd = System.getenv("ADMIN_PASSWORD");
        if (uri == null || adminPwd == null) {
            System.err.println("MONGODB_URI and ADMIN_PASSWORD required");
            System.exit(2);
        }

        try (MongoClient root = clientFor(uri, "root", adminPwd, "admin")) {
            root.getDatabase("admin").runCommand(new Document()
                .append("createUser", "cluster_mon").append("pwd", "p")
                .append("roles", List.of(new Document()
                    .append("role", "clusterMonitor").append("db", "admin"))));
            root.getDatabase("admin").runCommand(new Document()
                .append("createUser", "backup_user").append("pwd", "p")
                .append("roles", List.of(new Document()
                    .append("role", "backup").append("db", "admin"))));
            root.getDatabase("shop").getCollection("items")
                .insertOne(new Document("_id", 1).append("name", "thing"));
        }

        // 1. clusterMonitor: listDatabases ok, insert rejected.
        try (MongoClient cm = clientFor(uri, "cluster_mon", "p", "admin")) {
            Document ldb = cm.getDatabase("admin").runCommand(new Document("listDatabases", 1));
            if (ldb.getDouble("ok") != 1.0 || !(ldb.get("databases") instanceof List)) {
                System.err.printf("clusterMonitor listDatabases: %s%n", ldb.toJson());
                System.exit(1);
            }
            int code = tryInsertExpectError(cm, "shop", "items", 99);
            if (code != 13) {
                System.err.printf("clusterMonitor insert code=%d, want 13%n", code);
                System.exit(1);
            }
        }

        // 2. backup: read on every db ok, insert rejected.
        try (MongoClient bk = clientFor(uri, "backup_user", "p", "admin")) {
            List<Document> docs = new ArrayList<>();
            bk.getDatabase("shop").getCollection("items").find(new Document()).into(docs);
            if (docs.size() != 1 || docs.get(0).getInteger("_id") != 1) {
                System.err.printf("backup read: %s%n", docs);
                System.exit(1);
            }
            int code = tryInsertExpectError(bk, "shop", "items", 99);
            if (code != 13) {
                System.err.printf("backup insert code=%d, want 13%n", code);
                System.exit(1);
            }
        }

        System.out.println("OK");
    }

    private static int tryInsertExpectError(MongoClient client, String db, String coll, int id) {
        try {
            client.getDatabase(db).getCollection(coll).insertOne(new Document("_id", id));
        } catch (MongoCommandException ex) {
            return ex.getErrorCode();
        } catch (MongoWriteException ex) {
            return ex.getError().getCode();
        }
        return 0;
    }

    private static MongoClient clientFor(String uri, String user, String pwd, String authSource) {
        MongoCredential credential = MongoCredential.createScramSha256Credential(
            user, authSource, pwd.toCharArray());
        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .credential(credential)
            .applyToClusterSettings(b -> b.serverSelectionTimeout(30, TimeUnit.SECONDS))
            .build();
        return MongoClients.create(settings);
    }
}
