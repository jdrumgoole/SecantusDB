package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.MongoCommandException;
import com.mongodb.MongoCredential;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import org.bson.Document;

import java.util.Arrays;
import java.util.List;

/**
 * Cross-driver custom roles smoke — Java.
 * <p>
 * As root: createRole "shopAuditor" with `find` on shop, create a
 * user bound to that role, then authenticate as the user and verify
 * `find` succeeds while `insert` is rejected (action not in the role's
 * privileges). Then grantPrivilegesToRole adds insert on top of find;
 * a fresh connection sees the new privilege fire.
 */
public final class CustomRolesSmoke {

    private CustomRolesSmoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        String adminPwd = System.getenv("ADMIN_PASSWORD");
        if (uri == null || adminPwd == null) {
            System.err.println("MONGODB_URI and ADMIN_PASSWORD required");
            System.exit(2);
        }

        try (MongoClient root = clientFor(uri, "root", adminPwd, "admin")) {
            root.getDatabase("shop").runCommand(new Document()
                .append("createRole", "shopAuditor")
                .append("privileges", List.of(new Document()
                    .append("resource", new Document()
                        .append("db", "shop")
                        .append("collection", ""))
                    .append("actions", List.of("find"))))
                .append("roles", List.of()));

            root.getDatabase("shop").runCommand(new Document()
                .append("createUser", "auditor_java")
                .append("pwd", "p")
                .append("roles", List.of(new Document()
                    .append("role", "shopAuditor")
                    .append("db", "shop"))));
        }

        try (MongoClient auditor = clientFor(uri, "auditor_java", "p", "shop")) {
            auditor.getDatabase("shop").getCollection("items")
                .find(new Document()).into(new java.util.ArrayList<>());

            MongoCommandException caught = null;
            try {
                auditor.getDatabase("shop").getCollection("items")
                    .insertOne(new Document("x", 1));
            } catch (MongoCommandException ex) {
                caught = ex;
            }
            if (caught == null) {
                System.err.println("insert should have been rejected for find-only role");
                System.exit(1);
            }
            String message = caught.getErrorMessage() == null ? "" : caught.getErrorMessage();
            if (caught.getErrorCode() != 13 && !message.contains("Unauthorized")) {
                System.err.printf("unexpected error: code=%d message=%s%n",
                    caught.getErrorCode(), message);
                System.exit(1);
            }
        }

        // grantPrivilegesToRole adds insert; reconnect to pick up.
        try (MongoClient root2 = clientFor(uri, "root", adminPwd, "admin")) {
            root2.getDatabase("shop").runCommand(new Document()
                .append("grantPrivilegesToRole", "shopAuditor")
                .append("privileges", List.of(new Document()
                    .append("resource", new Document()
                        .append("db", "shop")
                        .append("collection", ""))
                    .append("actions", List.of("insert")))));
        }

        try (MongoClient auditor2 = clientFor(uri, "auditor_java", "p", "shop")) {
            auditor2.getDatabase("shop").getCollection("items")
                .insertOne(new Document("x", 2));
            System.out.println("OK");
        }
    }

    private static MongoClient clientFor(String uri, String user, String pwd, String authSource) {
        MongoCredential credential = MongoCredential.createScramSha256Credential(
            user, authSource, pwd.toCharArray());
        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .credential(credential)
            .applyToClusterSettings(b -> b.serverSelectionTimeout(30,
                java.util.concurrent.TimeUnit.SECONDS))
            .build();
        return MongoClients.create(settings);
    }
}
