package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.MongoCredential;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import org.bson.Document;

import java.util.List;

/**
 * Cross-driver SCRAM-SHA-1 smoke — Java.
 * <p>
 * Provisions a user with {@code mechanisms: [SCRAM-SHA-1]}, connects
 * with the explicit SCRAM-SHA-1 credential, and verifies
 * connectionStatus surfaces the authenticated user. Exercises the
 * legacy MD5-prepass + SHA-1 PBKDF2 path.
 */
public final class ScramSha1Smoke {

    private ScramSha1Smoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        String adminPwd = System.getenv("ADMIN_PASSWORD");
        if (uri == null || adminPwd == null) {
            System.err.println("MONGODB_URI and ADMIN_PASSWORD required");
            System.exit(2);
        }

        try (MongoClient root = MongoClients.create(buildSettings(uri,
                MongoCredential.createScramSha256Credential("root", "admin", adminPwd.toCharArray())))) {
            root.getDatabase("admin").runCommand(new Document()
                .append("createUser", "legacy_java")
                .append("pwd", "pass")
                .append("roles", List.of())
                .append("mechanisms", List.of("SCRAM-SHA-1")));
        }

        try (MongoClient cli = MongoClients.create(buildSettings(uri,
                MongoCredential.createScramSha1Credential("legacy_java", "admin", "pass".toCharArray())))) {
            Document status = cli.getDatabase("admin").runCommand(new Document("connectionStatus", 1));
            Document authInfo = status.get("authInfo", Document.class);
            if (authInfo == null) {
                System.err.println("connectionStatus: no authInfo");
                System.exit(1);
            }
            @SuppressWarnings("unchecked")
            List<Document> users = (List<Document>) authInfo.get("authenticatedUsers");
            if (users == null || users.stream().noneMatch(u -> "legacy_java".equals(u.getString("user")))) {
                System.err.printf("authenticatedUsers: %s%n", users);
                System.exit(1);
            }
            System.out.println("OK");
        }
    }

    private static MongoClientSettings buildSettings(String uri, MongoCredential credential) {
        return MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .credential(credential)
            .applyToClusterSettings(b -> b.serverSelectionTimeout(5,
                java.util.concurrent.TimeUnit.SECONDS))
            .build();
    }
}
