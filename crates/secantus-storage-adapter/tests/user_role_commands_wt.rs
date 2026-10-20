//! The user and role management commands over real WiredTiger: what they
//! store, what they list and what they refuse. Every expectation is what
//! mongod 8.2.11 answered to the same command
//! (`tools/probes/user_role_commands.py`, 2026-10-13).

mod common;

use bson::spec::BinarySubtype;
use bson::{doc, Bson, Document};
use common::{dispatch_full, with_wt};
use secantus_commands::CommandContext;

fn ok(c: &mut CommandContext, command: Document) -> Document {
    let reply = dispatch_full(&command, c);
    assert_eq!(reply.get_f64("ok"), Ok(1.0), "{command}: {reply}");
    reply
}

fn refused(c: &mut CommandContext, command: Document) -> (i32, String) {
    let reply = dispatch_full(&command, c);
    assert_eq!(reply.get_f64("ok"), Ok(0.0), "{command}: {reply}");
    (
        reply.get_i32("code").unwrap_or(0),
        reply.get_str("errmsg").unwrap_or("").to_string(),
    )
}

fn users(c: &mut CommandContext, command: Document) -> Vec<Document> {
    ok(c, command)
        .get_array("users")
        .expect("users")
        .iter()
        .map(|u| u.as_document().expect("a user").clone())
        .collect()
}

fn roles(c: &mut CommandContext, command: Document) -> Vec<Document> {
    ok(c, command)
        .get_array("roles")
        .expect("roles")
        .iter()
        .map(|r| r.as_document().expect("a role").clone())
        .collect()
}

fn keys(d: &Document) -> Vec<&str> {
    d.keys().map(String::as_str).collect()
}

fn on(c: &mut CommandContext, db: &str, command: Document) -> Document {
    let was = std::mem::replace(&mut c.db_name, db.to_string());
    let reply = dispatch_full(&command, c);
    c.db_name = was;
    reply
}

/// A user is listed in mongod's field order with a UUID `userId`, its
/// custom data, and `mechanisms` last; its credentials only when asked for.
#[test]
fn users_info_lists_mongod_shape() {
    with_wt(|c| {
        ok(
            c,
            doc! {"createUser": "a", "pwd": "pw", "roles": ["read", {"role": "dbAdmin", "db": "other"}],
            "customData": {"k": 1}},
        );
        ok(
            c,
            doc! {"createUser": "b", "pwd": "pw", "roles": [], "mechanisms": ["SCRAM-SHA-256"]},
        );
        let listed = users(c, doc! {"usersInfo": 1});
        assert_eq!(listed.len(), 2);
        let a = &listed[0];
        assert_eq!(
            keys(a),
            [
                "_id",
                "userId",
                "user",
                "db",
                "customData",
                "roles",
                "mechanisms"
            ]
        );
        assert_eq!(a.get_str("_id"), Ok("t.a"));
        match a.get("userId") {
            Some(Bson::Binary(b)) => {
                assert_eq!((b.subtype, b.bytes.len()), (BinarySubtype::Uuid, 16))
            }
            other => panic!("userId: {other:?}"),
        }
        assert_eq!(
            a.get_array("mechanisms").unwrap(),
            &vec![
                Bson::String("SCRAM-SHA-1".into()),
                Bson::String("SCRAM-SHA-256".into())
            ]
        );
        assert_eq!(
            listed[1].get_array("mechanisms").unwrap(),
            &vec![Bson::String("SCRAM-SHA-256".into())]
        );

        let shown = users(c, doc! {"usersInfo": "a", "showCredentials": true});
        assert_eq!(
            keys(&shown[0]),
            [
                "_id",
                "userId",
                "user",
                "db",
                "credentials",
                "customData",
                "roles",
                "mechanisms"
            ]
        );
        let credentials = shown[0].get_document("credentials").unwrap();
        assert_eq!(keys(credentials), ["SCRAM-SHA-1", "SCRAM-SHA-256"]);
        assert_eq!(
            credentials
                .get_document("SCRAM-SHA-1")
                .unwrap()
                .get_i32("iterationCount"),
            Ok(10000)
        );
        assert_eq!(
            credentials
                .get_document("SCRAM-SHA-256")
                .unwrap()
                .get_i32("iterationCount"),
            Ok(15000)
        );

        let hidden = users(c, doc! {"usersInfo": "a", "showCustomData": false});
        assert_eq!(
            keys(&hidden[0]),
            ["_id", "userId", "user", "db", "roles", "mechanisms"]
        );

        // A filter is a query over the listed documents.
        assert_eq!(
            users(c, doc! {"usersInfo": 1, "filter": {"roles.role": "read"}}).len(),
            1
        );
        assert_eq!(
            users(c, doc! {"usersInfo": "a", "filter": {"user": "b"}}).len(),
            0
        );
        assert_eq!(
            refused(c, doc! {"usersInfo": 1, "filter": {"$bogus": 1}}).0,
            2
        );
        assert_eq!(
            refused(c, doc! {"usersInfo": []}).1,
            "$or argument must be a non-empty array"
        );
        assert_eq!(
            refused(c, doc! {"usersInfo": {"forAllDBs": false}}).1,
            "UserName contains an unknown field named: 'forAllDBs"
        );
    });
}

/// With `showPrivileges` a user is described: mechanisms at the
/// credentials' place, then what its roles give it, each role's privileges
/// merged by resource.
#[test]
fn users_info_show_privileges_describes_inheritance() {
    with_wt(|c| {
        ok(
            c,
            doc! {"createRole": "a", "privileges": [{"resource": {"db": "t", "collection": "c"}, "actions": ["update", "find"]}], "roles": ["read"]},
        );
        ok(
            c,
            doc! {"createRole": "b", "privileges": [], "roles": ["a"]},
        );
        ok(c, doc! {"createUser": "u", "pwd": "pw", "roles": ["b"]});
        let described = users(c, doc! {"usersInfo": "u", "showPrivileges": true});
        let u = &described[0];
        assert_eq!(
            keys(u),
            [
                "_id",
                "userId",
                "user",
                "db",
                "mechanisms",
                "roles",
                "inheritedRoles",
                "inheritedPrivileges",
                "inheritedAuthenticationRestrictions"
            ]
        );
        let mut inherited: Vec<String> = u
            .get_array("inheritedRoles")
            .unwrap()
            .iter()
            .map(|r| {
                r.as_document()
                    .unwrap()
                    .get_str("role")
                    .unwrap()
                    .to_string()
            })
            .collect();
        inherited.sort();
        assert_eq!(inherited, ["a", "b", "read"]);
        let privileges = u.get_array("inheritedPrivileges").unwrap();
        let on_c = privileges
            .iter()
            .map(|p| p.as_document().unwrap())
            .find(|p| p.get_document("resource").unwrap().get_str("collection") == Ok("c"))
            .expect("the privilege on c");
        assert_eq!(
            on_c.get_array("actions").unwrap(),
            &vec![Bson::String("find".into()), Bson::String("update".into())]
        );
        assert_eq!(privileges.len(), 3, "{privileges:?}");

        assert_eq!(
            refused(c, doc! {"usersInfo": 1, "showPrivileges": true}).1,
            "Privilege or restriction details require exact-match usersInfo queries"
        );
        let restricted = users(
            c,
            doc! {"usersInfo": "u", "showAuthenticationRestrictions": true},
        );
        assert_eq!(
            restricted[0]
                .get_array("authenticationRestrictions")
                .unwrap(),
            &Vec::<Bson>::new()
        );
    });
}

/// `createUser`'s refusals, in mongod's words and codes.
#[test]
fn create_user_refusals() {
    with_wt(|c| {
        ok(c, doc! {"createUser": "a", "pwd": "pw", "roles": ["read"]});
        for (command, code, errmsg) in [
            (
                doc! {"createUser": "a", "pwd": "pw", "roles": []},
                51003,
                "User \"a@t\" already exists",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw", "roles": 1},
                14,
                "BSON field 'createUser.roles' is the wrong type 'int', expected type 'array'",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw"},
                40414,
                "BSON field 'createUser.roles' is missing but a required field",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw", "roles": [], "bogus": 1},
                40415,
                "BSON field 'createUser.bogus' is an unknown field.",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw", "roles": ["nosuch"]},
                31,
                "Could not find role: nosuch@t",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw", "roles": ["root"]},
                31,
                "Could not find role: root@t",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw", "roles": [{"role": "read"}]},
                2,
                "RoleName must contain a field named: db",
            ),
            (
                doc! {"createUser": "b", "roles": []},
                2,
                "Must provide a 'pwd' field for all user documents, except those with '$external' as the user's source db",
            ),
            (
                doc! {"createUser": "b", "pwd": "", "roles": []},
                50687,
                "Error preflighting UTF-8 conversion: U_STRING_NOT_TERMINATED_WARNING",
            ),
            (
                doc! {"createUser": "", "pwd": "pw", "roles": []},
                2,
                "User document needs 'user' field to be non-empty",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw", "roles": [], "mechanisms": []},
                2,
                "mechanisms field must not be empty",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw", "roles": [], "mechanisms": ["MONGODB-X509"]},
                2,
                "Unknown auth mechanism 'MONGODB-X509'",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw", "roles": [], "digestPassword": false},
                2,
                "Use of SCRAM-SHA-256 requires undigested passwords",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw", "roles": [], "authenticationRestrictions": [{"bogus": 1}]},
                40415,
                "BSON field 'createUser.authenticationRestrictions.bogus' is an unknown field.",
            ),
            (
                doc! {"createUser": "b", "pwd": "pw", "roles": [], "authenticationRestrictions": [{}]},
                162,
                "At least one of 'clientSource' or 'serverAddress' must be set",
            ),
        ] {
            assert_eq!(refused(c, command.clone()), (code, errmsg.to_string()), "{command}");
        }
        assert_eq!(
            on(
                c,
                "local",
                doc! {"createUser": "l", "pwd": "pw", "roles": []}
            )
            .get_str("errmsg"),
            Ok("Cannot create users in the local database")
        );
        assert_eq!(
            on(
                c,
                "$external",
                doc! {"createUser": "CN=x", "pwd": "pw", "roles": []}
            )
            .get_str("errmsg"),
            Ok("Cannot set the password for users defined on the '$external' database")
        );
        assert_eq!(
            on(c, "", doc! {"createUser": "x", "pwd": "pw", "roles": []}).get_i32("code"),
            Ok(73)
        );
    });
}

/// A `$external` user has no password: its credentials are `external` and
/// so is its one mechanism.
#[test]
fn external_users_have_no_password() {
    with_wt(|c| {
        assert_eq!(
            on(c, "$external", doc! {"createUser": "CN=y", "roles": []}).get_f64("ok"),
            Ok(1.0)
        );
        let reply = on(
            c,
            "$external",
            doc! {"usersInfo": 1, "showCredentials": true},
        );
        let user = reply.get_array("users").unwrap()[0]
            .as_document()
            .unwrap()
            .clone();
        assert_eq!(
            user.get_document("credentials").unwrap(),
            &doc! {"external": true}
        );
        assert_eq!(
            user.get_array("mechanisms").unwrap(),
            &vec![Bson::String("external".into())]
        );
    });
}

/// `updateUser` changes what it is given and refuses what mongod refuses;
/// a mechanism list without a password keeps only those credentials.
#[test]
fn update_user_changes_fields_in_place() {
    with_wt(|c| {
        ok(
            c,
            doc! {"createUser": "a", "pwd": "pw", "roles": ["read"], "customData": {"k": 1}},
        );
        assert_eq!(
            refused(c, doc! {"updateUser": "a"}).1,
            "Must specify at least one field to update in updateUser"
        );
        assert_eq!(
            refused(c, doc! {"updateUser": "a", "bogus": 1}).1,
            "BSON field 'updateUser.bogus' is an unknown field."
        );
        assert_eq!(
            refused(c, doc! {"updateUser": "nosuch", "roles": []}),
            (11, "User nosuch@t not found".to_string())
        );
        assert_eq!(
            refused(c, doc! {"updateUser": "a", "mechanisms": ["bogus"]}).1,
            "mechanisms field must be a subset of previously set mechanisms"
        );
        ok(
            c,
            doc! {"updateUser": "a", "customData": {"k": 2}, "roles": []},
        );
        let a = &users(c, doc! {"usersInfo": "a"})[0];
        assert_eq!(a.get_document("customData"), Ok(&doc! {"k": 2}));
        assert_eq!(a.get_array("roles").unwrap(), &Vec::<Bson>::new());

        ok(c, doc! {"updateUser": "a", "mechanisms": ["SCRAM-SHA-256"]});
        ok(
            c,
            doc! {"updateUser": "a", "mechanisms": ["SCRAM-SHA-1"], "pwd": "x"},
        );
        let a = &users(c, doc! {"usersInfo": "a", "showCredentials": true})[0];
        assert_eq!(
            keys(a.get_document("credentials").unwrap()),
            ["SCRAM-SHA-1"]
        );
        assert_eq!(
            a.get_array("mechanisms").unwrap(),
            &vec![Bson::String("SCRAM-SHA-1".into())]
        );
        assert_eq!(
            refused(
                c,
                doc! {"updateUser": "a", "digestPassword": false, "pwd": "x"}
            )
            .1,
            "Use of SCRAM-SHA-256 requires undigested passwords"
        );
    });
}

/// `dropUser`, the role grants and `dropAllUsersFromDatabase`, each with
/// mongod's own `UserNotFound` wording.
#[test]
fn drop_and_grant_name_the_user_as_mongod_does() {
    with_wt(|c| {
        ok(c, doc! {"createUser": "a", "pwd": "pw", "roles": ["read"]});
        ok(c, doc! {"createUser": "b", "pwd": "pw", "roles": []});
        assert_eq!(
            refused(c, doc! {"dropUser": "nosuch"}),
            (11, "User 'nosuch@t' not found".to_string())
        );
        assert_eq!(refused(c, doc! {"dropUser": "b", "bogus": 1}).0, 40415);
        ok(c, doc! {"dropUser": "b", "writeConcern": {"w": 1}});
        assert_eq!(
            refused(c, doc! {"grantRolesToUser": "nosuch", "roles": ["read"]}),
            (
                11,
                "Could not find user \"nosuch\" for db \"t\"".to_string()
            )
        );
        assert_eq!(
            refused(c, doc! {"grantRolesToUser": "a", "roles": []}).1,
            "grantRolesToUser command requires a non-empty \"roles\" array"
        );
        assert_eq!(refused(c, doc! {"grantRolesToUser": "a"}).0, 40414);
        ok(
            c,
            doc! {"grantRolesToUser": "a", "roles": ["readWrite", {"role": "dbAdmin", "db": "other"}]},
        );
        ok(c, doc! {"revokeRolesFromUser": "a", "roles": ["read"]});
        assert_eq!(
            refused(
                c,
                doc! {"revokeRolesFromUser": "a", "roles": ["nosuchrole"]}
            ),
            (31, "Could not find role: nosuchrole@t".to_string())
        );
        let held: Vec<String> = users(c, doc! {"usersInfo": "a"})[0]
            .get_array("roles")
            .unwrap()
            .iter()
            .map(|r| {
                r.as_document()
                    .unwrap()
                    .get_str("role")
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(held, ["readWrite", "dbAdmin"]);

        let dropped = ok(c, doc! {"dropAllUsersFromDatabase": 1});
        assert_eq!(keys(&dropped), ["n", "ok"]);
        assert_eq!(dropped.get_i32("n"), Ok(1));
        assert_eq!(
            refused(c, doc! {"dropAllUsersFromDatabase": 1, "bogus": 1}).0,
            40415
        );
    });
}

/// A role's privileges come back one per resource, actions in mongod's
/// order; `rolesInfo` shows them only when asked and words a listed role,
/// a named role and a built-in role each differently.
#[test]
fn roles_info_shapes() {
    with_wt(|c| {
        ok(
            c,
            doc! {"createRole": "a", "privileges": [
                {"resource": {"db": "t", "collection": "c"}, "actions": ["update", "find", "find"]},
                {"resource": {"db": "t", "collection": "c"}, "actions": ["insert"]},
            ], "roles": ["read"]},
        );
        let named = &roles(c, doc! {"rolesInfo": "a"})[0];
        assert_eq!(
            keys(named),
            ["_id", "role", "db", "roles", "inheritedRoles", "isBuiltin"]
        );
        assert_eq!(named.get_bool("isBuiltin"), Ok(false));

        let listed = &roles(c, doc! {"rolesInfo": 1})[0];
        assert_eq!(
            keys(listed),
            ["_id", "role", "db", "roles", "isBuiltin", "inheritedRoles"]
        );

        let shown = &roles(c, doc! {"rolesInfo": "a", "showPrivileges": true})[0];
        assert_eq!(
            keys(shown),
            [
                "_id",
                "role",
                "db",
                "privileges",
                "roles",
                "inheritedRoles",
                "inheritedPrivileges",
                "isBuiltin"
            ]
        );
        assert_eq!(
            shown.get_array("privileges").unwrap(),
            &vec![Bson::Document(
                doc! {"resource": {"db": "t", "collection": "c"}, "actions": ["find", "insert", "update"]}
            )]
        );
        // Its own privileges as they are, then what `read` gives.
        assert_eq!(shown.get_array("inheritedPrivileges").unwrap().len(), 3);

        let builtin = &roles(c, doc! {"rolesInfo": "read"})[0];
        assert_eq!(
            builtin,
            &doc! {"db": "t", "role": "read", "roles": [], "inheritedRoles": [], "isBuiltin": true}
        );
        let builtin_shown = &roles(c, doc! {"rolesInfo": "read", "showPrivileges": true})[0];
        assert_eq!(
            keys(builtin_shown),
            [
                "db",
                "role",
                "roles",
                "privileges",
                "inheritedRoles",
                "inheritedPrivileges",
                "isBuiltin"
            ]
        );
        let read_privileges = builtin_shown.get_array("privileges").unwrap();
        assert_eq!(read_privileges.len(), 2);
        assert_eq!(
            read_privileges[0]
                .as_document()
                .unwrap()
                .get_document("resource"),
            Ok(&doc! {"db": "t", "collection": ""})
        );
        assert!(roles(c, doc! {"rolesInfo": "root"}).is_empty());
        assert_eq!(
            on(c, "admin", doc! {"rolesInfo": "root"})
                .get_array("roles")
                .unwrap()
                .len(),
            1
        );

        let with_builtins = roles(c, doc! {"rolesInfo": 1, "showBuiltinRoles": true});
        assert_eq!(with_builtins.len(), 7);
        assert_eq!(
            keys(&with_builtins[0]),
            ["role", "db", "isBuiltin", "roles", "inheritedRoles"]
        );
        assert_eq!(with_builtins[6].get_str("role"), Ok("a"));
        assert_eq!(
            on(c, "admin", doc! {"rolesInfo": 1, "showBuiltinRoles": true})
                .get_array("roles")
                .unwrap()
                .len(),
            21
        );

        let fragment = ok(
            c,
            doc! {"rolesInfo": ["a", "nosuch"], "showPrivileges": "asUserFragment"},
        );
        let fragment = fragment.get_document("userFragment").unwrap();
        assert_eq!(
            keys(fragment),
            ["roles", "inheritedRoles", "inheritedPrivileges"]
        );
        assert_eq!(fragment.get_array("inheritedRoles").unwrap().len(), 3);
        assert_eq!(
            refused(c, doc! {"rolesInfo": 1, "showPrivileges": "asUserFragment"}),
            (
                20,
                "Cannot get user fragment for all roles in a database".to_string()
            )
        );
        assert_eq!(
            refused(c, doc! {"rolesInfo": {"forAllDBs": true}}).1,
            "RoleName must contain a field named: role"
        );
    });
}

/// What `createRole` refuses: a built-in's name, another database's
/// privileges or roles, a cycle, an unknown action, a malformed resource.
#[test]
fn create_role_refusals() {
    with_wt(|c| {
        ok(c, doc! {"createRole": "a", "privileges": [], "roles": []});
        for (command, code, errmsg) in [
            (
                doc! {"createRole": "a", "privileges": [], "roles": []},
                51002,
                "Role \"a@t\" already exists",
            ),
            (
                doc! {"createRole": "read", "privileges": [], "roles": []},
                2,
                "Cannot create roles with the same name as a built-in role",
            ),
            (
                doc! {"createRole": "", "privileges": [], "roles": []},
                2,
                "Role name must be non-empty",
            ),
            (
                doc! {"createRole": "b", "roles": []},
                40414,
                "BSON field 'createRole.privileges' is missing but a required field",
            ),
            (
                doc! {"createRole": "b", "privileges": [1], "roles": []},
                14,
                "BSON field 'createRole.privileges.0' is the wrong type 'int', expected type 'object'",
            ),
            (
                doc! {"createRole": "b", "privileges": [{"resource": {"db": "t", "collection": "c"}}], "roles": []},
                40414,
                "BSON field 'createRole.privileges.actions' is missing but a required field",
            ),
            (
                doc! {"createRole": "b", "privileges": [{"resource": {"db": "t"}, "actions": ["find"]}], "roles": []},
                2,
                "resource pattern must contain 'collection' or 'systemBuckets' specifier",
            ),
            (
                doc! {"createRole": "b", "privileges": [{"resource": {"db": "t", "collection": "c"}, "actions": ["bogusAction"]}], "roles": []},
                2,
                "Unknown action type in privilege set: 'bogusAction'",
            ),
            (
                doc! {"createRole": "b", "privileges": [{"resource": {"db": "t", "collection": "c"}, "actions": []}], "roles": []},
                2,
                "'actions' field of privilege resource must not be empty",
            ),
            (
                doc! {"createRole": "b", "privileges": [{"resource": {"db": "other", "collection": "c"}, "actions": ["find"]}], "roles": []},
                49,
                "Roles on the 't' database cannot be granted privileges that target other databases or the cluster",
            ),
            (
                doc! {"createRole": "b", "privileges": [{"resource": {"cluster": true}, "actions": ["serverStatus"]}], "roles": []},
                49,
                "Roles on the 't' database cannot be granted privileges that target other databases or the cluster",
            ),
            (
                doc! {"createRole": "b", "privileges": [], "roles": [{"role": "read", "db": "other"}]},
                49,
                "Roles on the 't' database cannot be granted roles from other databases",
            ),
            (
                doc! {"createRole": "b", "privileges": [], "roles": ["b"]},
                49,
                "Cannot grant role b@t to itself.",
            ),
            (
                doc! {"createRole": "b", "privileges": [], "roles": ["nosuchrole"]},
                31,
                "Cannot grant roles to 'b@t': Could not find role: nosuchrole@t",
            ),
        ] {
            assert_eq!(refused(c, command.clone()), (code, errmsg.to_string()), "{command}");
        }
        // `root` is a built-in of `admin` only, so another database may
        // define it; and `admin` may hold other databases' roles and the
        // cluster's privileges.
        ok(
            c,
            doc! {"createRole": "root", "privileges": [], "roles": []},
        );
        assert_eq!(
            on(c, "admin", doc! {"createRole": "admrole", "privileges": [{"resource": {"cluster": true}, "actions": ["serverStatus"]}], "roles": [{"role": "read", "db": "t"}]}).get_f64("ok"),
            Ok(1.0)
        );
        assert_eq!(
            on(
                c,
                "local",
                doc! {"createRole": "l", "privileges": [], "roles": []}
            )
            .get_str("errmsg"),
            Ok("Cannot create roles in the local database")
        );
    });
}

/// `updateRole`, the grant / revoke quartet and `dropRole`: cycles, the
/// built-ins, the words for a role that does not exist, and what a drop
/// takes away from whom.
#[test]
fn update_grant_revoke_and_drop_roles() {
    with_wt(|c| {
        ok(
            c,
            doc! {"createRole": "a", "privileges": [{"resource": {"db": "t", "collection": "c"}, "actions": ["find"]}], "roles": ["read"]},
        );
        ok(
            c,
            doc! {"createRole": "b", "privileges": [], "roles": ["a"]},
        );
        ok(c, doc! {"createRole": "c", "privileges": [], "roles": []});
        ok(
            c,
            doc! {"createUser": "u", "pwd": "pw", "roles": ["a", "b"]},
        );

        assert_eq!(
            refused(c, doc! {"updateRole": "a"}).1,
            "Must specify at least one field to update in updateRole"
        );
        assert_eq!(
            refused(c, doc! {"updateRole": "a", "roles": ["b"]}),
            (
                49,
                "Granting roles to a@t would introduce a cycle in the role graph".to_string()
            )
        );
        assert_eq!(
            refused(c, doc! {"updateRole": "nosuch", "roles": []}),
            (31, "Could not find role: nosuch@t".to_string())
        );
        assert_eq!(
            refused(c, doc! {"updateRole": "read", "roles": []}),
            (31, "Role read@t not found".to_string())
        );
        ok(
            c,
            doc! {"updateRole": "a", "privileges": [{"resource": {"db": "t", "collection": ""}, "actions": ["insert", "update"]}], "roles": []},
        );
        let a = &roles(c, doc! {"rolesInfo": "a", "showPrivileges": true})[0];
        assert_eq!(a.get_array("inheritedRoles").unwrap(), &Vec::<Bson>::new());
        assert_eq!(a.get_array("privileges").unwrap().len(), 1);

        assert_eq!(
            refused(c, doc! {"grantPrivilegesToRole": "a", "privileges": []}).1,
            "grantPrivilegesToRole command requires a non-empty \"privileges\" array"
        );
        assert_eq!(
            refused(
                c,
                doc! {"grantPrivilegesToRole": "read", "privileges": [{"resource": {"db": "t", "collection": "c"}, "actions": ["find"]}]}
            ),
            (
                2,
                "read@t is a built-in role and cannot be modified".to_string()
            )
        );
        assert_eq!(
            refused(
                c,
                doc! {"grantPrivilegesToRole": "nosuch", "privileges": [{"resource": {"db": "t", "collection": "c"}, "actions": ["find"]}]}
            ),
            (31, "Role nosuch@t not found".to_string())
        );
        ok(
            c,
            doc! {"grantPrivilegesToRole": "a", "privileges": [{"resource": {"db": "t", "collection": ""}, "actions": ["remove", "find"]}]},
        );
        let a = &roles(c, doc! {"rolesInfo": "a", "showPrivileges": true})[0];
        assert_eq!(
            a.get_array("privileges").unwrap()[0]
                .as_document()
                .unwrap()
                .get_array("actions")
                .unwrap(),
            &["find", "insert", "remove", "update"]
                .map(|a| Bson::String(a.into()))
                .to_vec()
        );
        ok(
            c,
            doc! {"revokePrivilegesFromRole": "a", "privileges": [{"resource": {"db": "t", "collection": ""}, "actions": ["find", "insert", "remove", "update"]}]},
        );
        assert_eq!(
            roles(c, doc! {"rolesInfo": "a", "showPrivileges": true})[0]
                .get_array("privileges")
                .unwrap(),
            &Vec::<Bson>::new()
        );

        assert_eq!(
            refused(c, doc! {"grantRolesToRole": "a", "roles": ["a"]}),
            (49, "Cannot grant role a@t to itself.".to_string())
        );
        assert_eq!(
            refused(c, doc! {"grantRolesToRole": "a", "roles": ["b"]}),
            (
                49,
                "Granting roles to a@t would introduce a cycle in the role graph".to_string()
            )
        );
        assert_eq!(
            refused(c, doc! {"grantRolesToRole": "a", "roles": ["nosuchrole"]}),
            (
                31,
                "Cannot grant roles to 'a@t': Could not find role: nosuchrole@t".to_string()
            )
        );
        ok(c, doc! {"grantRolesToRole": "a", "roles": ["c", "read"]});
        ok(
            c,
            doc! {"revokeRolesFromRole": "a", "roles": ["read", "nosuchrole"]},
        );
        assert_eq!(
            roles(c, doc! {"rolesInfo": "a"})[0]
                .get_array("roles")
                .unwrap(),
            &vec![Bson::Document(doc! {"role": "c", "db": "t"})]
        );

        assert_eq!(
            refused(c, doc! {"dropRole": "read"}),
            (
                2,
                "read@t is a built-in role and cannot be modified".to_string()
            )
        );
        ok(c, doc! {"dropRole": "c"});
        assert_eq!(
            refused(c, doc! {"dropRole": "c"}),
            (31, "Could not find role: c@t".to_string())
        );
        // The dropped role is gone from the roles and users that held it...
        assert_eq!(
            roles(c, doc! {"rolesInfo": "a"})[0]
                .get_array("roles")
                .unwrap(),
            &Vec::<Bson>::new()
        );
        ok(c, doc! {"dropRole": "b"});
        assert_eq!(
            users(c, doc! {"usersInfo": "u"})[0]
                .get_array("roles")
                .unwrap(),
            &vec![Bson::Document(doc! {"role": "a", "db": "t"})]
        );
        // ... while `dropAllRolesFromDatabase` leaves the user naming it.
        let dropped = ok(c, doc! {"dropAllRolesFromDatabase": 1});
        assert_eq!(keys(&dropped), ["n", "ok"]);
        assert_eq!(dropped.get_i32("n"), Ok(1));
        assert_eq!(
            users(c, doc! {"usersInfo": "u"})[0]
                .get_array("roles")
                .unwrap()
                .len(),
            1
        );
        let described = &users(c, doc! {"usersInfo": "u", "showPrivileges": true})[0];
        assert_eq!(described.get_array("inheritedRoles").unwrap().len(), 1);
        assert_eq!(
            described.get_array("inheritedPrivileges").unwrap(),
            &Vec::<Bson>::new()
        );
    });
}

/// A user stored before records carried a `userId` gets one when the server
/// starts, after `_id` as mongod keeps it, and `usersInfo` lists it (RM-031).
#[test]
fn a_stored_user_without_a_user_id_gets_one() {
    with_wt(|c| {
        let old = doc! {
            "_id": "test.old",
            "user": "old",
            "db": "test",
            "credentials": {},
            "roles": [],
        };
        let storage = c.storage.clone().expect("a store");
        storage
            .add_user("test", "old", &bson::to_vec(&old).unwrap(), false)
            .unwrap();
        assert_eq!(
            secantus_commands::auth::backfill_user_ids(storage.as_ref()).unwrap(),
            1
        );
        assert_eq!(
            secantus_commands::auth::backfill_user_ids(storage.as_ref()).unwrap(),
            0
        );
        let record = bson::Document::from_reader(
            &mut &storage.get_user("test", "old").unwrap().unwrap()[..],
        )
        .unwrap();
        let keys: Vec<&str> = record.keys().map(String::as_str).take(3).collect();
        assert_eq!(keys, ["_id", "userId", "user"]);
        c.db_name = "test".into();
        let reply = secantus_commands::dispatch(&doc! {"usersInfo": "old"}, c);
        let user = reply.get_array("users").unwrap()[0]
            .as_document()
            .unwrap()
            .clone();
        assert!(user.get_binary_generic("userId").is_err() && user.contains_key("userId"));
    });
}
