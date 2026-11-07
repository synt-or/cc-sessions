//! Le catalogue des catégories : les statuts de `cs`, avec le critère donné aux modèles.
//! « En attente » correspond à `hold`, « tokens à burn » à `ready_to_burn`.
pub const CATEGORIES: [(&str, &str); 5] = [
    ("active", "Travail en cours : la session continue maintenant ou très bientôt."),
    ("hold", "Encore utile mais en pause : à reprendre plus tard, sans date."),
    ("ready_to_burn", "Tokens à burn : travail prêt à être lancé dès que du quota est disponible."),
    ("needs_manual_work", "Attend une intervention manuelle de l'utilisateur pour avancer."),
    ("done", "Terminée : il ne reste rien à faire dans cette session."),
];
